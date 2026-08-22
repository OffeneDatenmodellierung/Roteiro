//! Cross-repo **config override matrix + drift view** (ADR-0009 step 7, "views").
//!
//! The workspace overview the prototype validated: a grid of the hub's config
//! keys against each spoke repo, showing which spoke overrides which key (and to
//! what), plus a **drift** list of spoke keys the hub doesn't define. Built from
//! the same inferred matches `roteiro links --infer` produces, then rendered as a
//! self-contained HTML page (`--html`), a text table, or JSON.
//!
//! Pure data + rendering: the caller feeds in the already-matched inputs (each
//! carrying its own [`Provenance`]), so this module has no store/workspace
//! dependencies and is fully unit-testable.

use std::collections::BTreeMap;

use rto_graph::Provenance;

/// One matched override fed into the matrix.
pub struct MatchInput {
    /// The hub config key this override maps to.
    pub hub_key: String,
    /// The source file the hub key was read from — the `<file>` component of the
    /// hub `config_key` node's `cfgkey:<file>#<dotted>` id. Carried onto the [`Row`]
    /// so a client (the explorer's "hide tooling config" toggle) and the CLI can
    /// classify the row as app vs tooling config. Empty when the source is unknown.
    pub file: String,
    /// The spoke's own key (its naming convention).
    pub spoke_key: String,
    /// The spoke's value for it.
    pub spoke_value: String,
    /// Match confidence in `0.0..=1.0` (meaningful only for inferred links; an
    /// authored link carries no score, so callers pass `0.0`).
    pub confidence: f64,
    /// How this override link was produced — [`Provenance::Authored`] (a declared
    /// `[[links]]`) or [`Provenance::Inferred`] (a confidence-scored match). The
    /// real per-cell provenance, carried onto the [`Cell`].
    pub provenance: Provenance,
}

/// One spoke's contribution to the matrix.
pub struct SpokeInput {
    /// The spoke project (repo dir name).
    pub name: String,
    /// Its matched overrides against the hub.
    pub matches: Vec<MatchInput>,
    /// Its orphan `(key, value)`s — no hub counterpart (the drift candidates).
    pub orphans: Vec<(String, String)>,
    /// The hub version this spoke was compared against, under `--pinned`
    /// (ADR-0009 step 8b). `None` means it was compared against the hub's `HEAD`
    /// — either because `--pinned` was not asked for, or because this spoke pins
    /// nothing detectable.
    pub pin: Option<SpokePin>,
}

/// Which hub version one spoke was resolved against, and where that came from.
///
/// Carried per spoke rather than per matrix because the whole point of
/// `--pinned` is that spokes differ: a matrix that resolved seven spokes against
/// seven revs and reported one number would be describing a comparison it did
/// not make.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpokePin {
    /// The git rev — a sha, a tag, any rev.
    pub rev: String,
    /// Where the pin was detected (e.g. `submodule vendor/app`), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
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
    /// How the override link was produced (authored vs inferred). The real
    /// per-cell provenance the UI colours by (gold authored / slate inferred),
    /// replacing the old confidence≥1.0 heuristic.
    pub provenance: Provenance,
    /// The spoke value differs from the hub's default — a *real* override, not a
    /// redundant restatement. The signal a reader scans for.
    pub differs: bool,
}

/// One hub key's row: its default value and each spoke's override of it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    /// The hub config key.
    pub hub_key: String,
    /// The source file the hub key was read from (the `<file>` in the hub
    /// `config_key` node's `cfgkey:<file>#<dotted>` id) — the classifier input for
    /// the "hide tooling config" filter (see [`MatchInput::file`]). Additive and
    /// backward-compatible: older clients simply ignore it. Empty when unknown.
    pub file: String,
    /// The hub's own value (the default the spokes override).
    pub hub_value: String,
    /// Overriding cell per spoke name (only spokes that override this key).
    pub cells: BTreeMap<String, Cell>,
}

/// One spoke's setting of an orphan (drift) key — the per-deploy column of a
/// [`Drift`] row, mirroring the override [`Cell`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct DriftCell {
    /// The spoke's value for the orphan key. When [`DriftCell::conflict`] is set,
    /// this is a deterministic, sorted `" | "`-join of every distinct value the
    /// deploy gave the key, so nothing is dropped.
    pub value: String,
    /// The same deploy set this drift key to two or more *different* non-empty
    /// values (e.g. from two files in that repo). Rather than an order-dependent
    /// silent overwrite, [`build`] surfaces the collision: `value` carries all the
    /// distinct values joined and the renderers flag the cell. `false` for the
    /// common single-value cell.
    pub conflict: bool,
}

/// A config key set by one or more spokes but with no hub counterpart — the drift
/// candidate. Grouped to exactly **one entry per distinct key** (mirroring [`Row`]):
/// every spoke that sets the key contributes a [`DriftCell`], so two deploys that
/// set the same key — even to different values — share a single row, each value
/// carried in its own deploy column rather than emitting a duplicate row per deploy.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Drift {
    /// The orphan key.
    pub key: String,
    /// Each spoke that sets this key, keyed by spoke name (only spokes that set it
    /// appear — mirrors [`Row::cells`]).
    pub cells: BTreeMap<String, DriftCell>,
}

/// The assembled cross-repo override matrix.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OverrideMatrix {
    /// The hub project (source of truth).
    pub hub: String,
    /// Whether per-spoke pin resolution was **asked for** (`--pinned`).
    ///
    /// Separate from `pins` being empty, and the distinction is the whole of
    /// #505: "we asked and none of them pinned anything" is a different claim
    /// from "we did not ask", and an inert `--pinned` that rendered
    /// byte-identically to a plain run would be the answer to a question nobody
    /// posed.
    pub pinned: bool,
    /// Spoke → the hub version it was compared against, for the spokes that
    /// pinned one. Absent for a spoke compared against the hub's `HEAD`.
    pub pins: BTreeMap<String, SpokePin>,
    /// Spoke column order — every deploy that either overrides at least one hub key
    /// *or* only drifts (sets a key with no hub counterpart), so a drift-only
    /// deploy still gets a column for its drift value. Sorted by name.
    pub spokes: Vec<String>,
    /// One row per overridden hub key, sorted by key.
    pub rows: Vec<Row>,
    /// Orphan drift keys, one row per distinct key (sorted by key), each carrying
    /// a per-spoke cell for every deploy that sets it.
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
    pinned: bool,
) -> OverrideMatrix {
    // Per deploy, the distinct non-empty values it gave a drift key (sorted, so the
    // cell renders deterministically — one value, or a flagged conflict).
    type DriftValues = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

    let pins: BTreeMap<String, SpokePin> = spokes
        .iter()
        .filter_map(|s| s.pin.clone().map(|p| (s.name.clone(), p)))
        .collect();

    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut columns: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Drift grouped by distinct key — one row per key (mirroring `rows`), with a
    // per-spoke cell for each deploy that sets it, instead of one row per
    // (spoke, key) occurrence (which duplicated a key set by N deploys into N rows).
    // The accumulated values become one value or a flagged conflict, never an
    // order-dependent overwrite.
    let mut drift: BTreeMap<String, DriftValues> = BTreeMap::new();
    // Hub keys whose matches disagreed on a source file. A `Row` is keyed by the
    // dotted `hub_key` alone, but the same dotted key can exist in more than one hub
    // file (a `config_key` node is keyed by `cfgkey:<file>#<dotted>`). If two matches
    // resolve the same `hub_key` to *different* non-empty files, the row's file is
    // ambiguous — record that so it can never be re-adopted from a later match, and
    // leave `row.file` empty (so the opt-in tooling filter treats it as app config
    // rather than hiding it on an arbitrary file).
    let mut ambiguous_file: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for spoke in spokes {
        for m in spoke.matches {
            let hub_value = hub_values.get(&m.hub_key).cloned().unwrap_or_default();
            let differs = hub_value != m.spoke_value;
            let row = rows.entry(m.hub_key.clone()).or_insert_with(|| Row {
                hub_key: m.hub_key.clone(),
                file: String::new(),
                hub_value: hub_value.clone(),
                cells: BTreeMap::new(),
            });
            // Reconcile the row's source file across all matches for this hub key:
            // adopt the first non-empty file, but clear it (permanently, via
            // `ambiguous_file`) the moment a later match reports a different one.
            if !m.file.is_empty() && !ambiguous_file.contains(&m.hub_key) {
                if row.file.is_empty() {
                    row.file.clone_from(&m.file);
                } else if row.file != m.file {
                    row.file.clear();
                    ambiguous_file.insert(m.hub_key.clone());
                }
            }
            let cell = Cell {
                value: m.spoke_value,
                spoke_key: m.spoke_key,
                confidence: m.confidence,
                provenance: m.provenance,
                differs,
            };
            // A spoke may set the same hub key in more than one file. Keep a *real*
            // override visible: never let a redundant restatement (`differs = false`)
            // overwrite a differing cell already recorded for this spoke+key.
            match row.cells.entry(spoke.name.clone()) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(cell);
                }
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    if cell.differs && !o.get().differs {
                        o.insert(cell);
                    }
                }
            }
            columns.insert(spoke.name.clone());
        }
        for (key, value) in spoke.orphans {
            // Record that this deploy set the key (so its column renders even for an
            // empty value) and accumulate its distinct non-empty values; the cell is
            // resolved once, below, so a re-stated blank never blanks a real value
            // and two differing values become a deterministic conflict, not a drop.
            let vals = drift
                .entry(key)
                .or_default()
                .entry(spoke.name.clone())
                .or_default();
            if !value.is_empty() {
                vals.insert(value);
            }
            // A deploy that only drifts (no override match) still needs a column so
            // its drift value has somewhere to render — mirror the override cells.
            columns.insert(spoke.name.clone());
        }
    }

    let drift = drift
        .into_iter()
        .map(|(key, spokes)| Drift {
            cells: spokes
                .into_iter()
                .map(|(spoke, values)| (spoke, drift_cell(values)))
                .collect(),
            key,
        })
        .collect();

    OverrideMatrix {
        hub: hub.to_owned(),
        pinned,
        pins,
        spokes: columns.into_iter().collect(),
        rows: rows.into_values().collect(),
        drift,
    }
}

/// The table's header row: the two fixed columns, then one per spoke carrying
/// that spoke's [`pin_label`].
fn html_head_row(m: &OverrideMatrix) -> String {
    use std::fmt::Write as _;
    let mut thead = String::from("<th scope=\"col\">config key</th><th scope=\"col\">hub</th>");
    for s in &m.spokes {
        let _ = write!(
            thead,
            "<th scope=\"col\">{}{}</th>",
            esc(s),
            pin_label(m, s)
        );
    }
    thead
}

/// The hub version one spoke's column was measured against, as an HTML fragment
/// for its header — empty unless `--pinned` asked for per-spoke resolution.
///
/// It belongs *in the column header* because that is where the ambiguity is:
/// under `--pinned` each column answers a different question, and a reader
/// comparing two cells side by side is otherwise comparing two hub versions
/// without being told. A spoke that pinned nothing is labelled rather than left
/// blank, for the same reason it is named rather than omitted in the text
/// output — a missing label reads as "not applicable", not as "compared against
/// HEAD" (#505).
fn pin_label(m: &OverrideMatrix, spoke: &str) -> String {
    if !m.pinned {
        return String::new();
    }
    match m.pins.get(spoke) {
        Some(p) => format!(
            "<span class=\"pin\" title=\"{}\">@ {}</span>",
            esc(p.via.as_deref().unwrap_or("pinned by this spoke")),
            esc(short_rev(&p.rev)),
        ),
        None => "<span class=\"pin unpinned\">@ HEAD (no pin)</span>".to_owned(),
    }
}

/// Abbreviate a 40-hex commit sha to 10 chars; leave short refs (tags) as-is.
///
/// Lives here rather than in `main` because both pin renderings need it — the
/// infer report's per-spoke line and this module's matrix header — and a sha
/// abbreviated to ten characters in one and forty in the other would read as two
/// different revisions.
#[must_use]
pub fn short_rev(rev: &str) -> &str {
    if rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        &rev[..10]
    } else {
        rev
    }
}

/// Collapse a deploy's distinct non-empty values for one drift key into a single
/// cell. Empty when the deploy only ever restated a blank value, the lone value
/// when it is consistent, or a deterministic sorted `" | "`-join flagged as a
/// `conflict` when the deploy set the key to two or more *different* non-empty
/// values (e.g. from two files) — surfaced, never silently dropped or made
/// iteration-order dependent (the input `BTreeSet` is already sorted).
fn drift_cell(values: std::collections::BTreeSet<String>) -> DriftCell {
    let conflict = values.len() > 1;
    DriftCell {
        value: values.into_iter().collect::<Vec<_>>().join(" | "),
        conflict,
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
    // Under `--pinned` the columns are not comparable to each other unless the
    // reader knows which hub version each was measured against — that is the
    // whole reason per-spoke pinning is worth having on the *matrix*, where
    // spokes sit side by side. Stated before the table, once.
    if m.pinned {
        let _ = writeln!(
            out,
            "  resolved per spoke against the hub version each pins \
             ({} of {} pinned one):",
            m.pins.len(),
            m.spokes.len()
        );
        for spoke in &m.spokes {
            match m.pins.get(spoke) {
                Some(p) => {
                    let via = p
                        .via
                        .as_deref()
                        .map_or_else(String::new, |v| format!(" (via {v})"));
                    let _ = writeln!(out, "    {spoke} @ {}{via}", short_rev(&p.rev));
                }
                // Named rather than omitted: a spoke silently missing from this
                // list reads as "not in the matrix", not as "compared against
                // HEAD" (#505).
                None => {
                    let _ = writeln!(out, "    {spoke} @ HEAD (no pin detected)");
                }
            }
        }
    }
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
            let _ = writeln!(out, "\n    {}", d.key);
            for (spoke, cell) in &d.cells {
                let flag = if cell.conflict { "  (conflict)" } else { "" };
                let _ = writeln!(out, "      {spoke}: {}{flag}", cell.value);
            }
        }
    }
    out
}

/// Render the matrix as a **self-contained** HTML page (inline CSS, no external
/// assets) — the `render web-graph` output: open it straight in a browser.
#[must_use]
pub fn render_html(m: &OverrideMatrix) -> String {
    use std::fmt::Write as _;
    let thead = html_head_row(m);

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
        // One row per distinct drift key, with a column per spoke that sets it —
        // mirroring the override matrix above rather than one row per (spoke, key).
        let mut dhead = String::from("<th scope=\"col\">key</th>");
        for s in &m.spokes {
            let _ = write!(dhead, "<th scope=\"col\">{}</th>", esc(s));
        }
        let mut rows = String::new();
        for d in &m.drift {
            let _ = write!(rows, "<tr><td><code>{}</code></td>", esc(&d.key));
            for spoke in &m.spokes {
                match d.cells.get(spoke) {
                    Some(cell) if cell.conflict => {
                        let _ = write!(
                            rows,
                            "<td class=\"cell over conflict\" \
                             title=\"conflict: this deploy sets the key to multiple values\">\
                             <code>{}</code></td>",
                            esc(&cell.value)
                        );
                    }
                    Some(cell) => {
                        let _ = write!(
                            rows,
                            "<td class=\"cell over\"><code>{}</code></td>",
                            esc(&cell.value)
                        );
                    }
                    None => rows.push_str("<td class=\"cell none\">·</td>"),
                }
            }
            rows.push_str("</tr>");
        }
        format!(
            "<h2>Drift — {} orphan key(s)</h2>\
             <p class=\"muted\">Spoke keys with no hub counterpart: the app doesn't \
             define these, so a rename or removal in the hub can't warn you.</p>\
             <table class=\"drift\"><thead><tr>{dhead}</tr></thead>\
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
.pin{display:block;margin-top:.15rem;font-size:.7rem;text-transform:none;\
letter-spacing:0;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;\
font-weight:400}\
.pin.unpinned{opacity:.55;font-style:italic}\
.legend{color:var(--muted);font-size:.85rem;margin:1rem 0}\
.swatch{display:inline-block;width:.8rem;height:.8rem;border-radius:3px;\
vertical-align:-1px;border:1px solid var(--line)}\
.swatch.over{background:var(--over)}.swatch.same{background:var(--same)}\
.swatch.none{background:var(--bg)}\
table.drift td:first-child{white-space:nowrap;color:var(--muted)}\
td.conflict{outline:2px solid var(--over-fg);outline-offset:-2px;font-weight:600}";

/// Escape text for HTML body or attribute content (both quote styles), so the
/// helper stays safe if reused inside single-quoted attributes.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same fixture as [`matrix`], but resolved per spoke against the hub
    /// version each pins — two spokes on different revs, one pinning nothing.
    fn pinned_matrix() -> OverrideMatrix {
        let hub_values = BTreeMap::from([("serve.addr".to_owned(), "127.0.0.1:8017".to_owned())]);
        let spoke = |name: &str, value: &str, pin: Option<SpokePin>| SpokeInput {
            name: name.to_owned(),
            matches: vec![MatchInput {
                hub_key: "serve.addr".to_owned(),
                file: "config.toml".to_owned(),
                spoke_key: "SERVE_ADDR".to_owned(),
                spoke_value: value.to_owned(),
                confidence: 0.9,
                provenance: Provenance::Inferred,
            }],
            orphans: vec![],
            pin,
        };
        build(
            "app",
            &hub_values,
            vec![
                spoke(
                    "deploy-a",
                    "0.0.0.0:8443",
                    Some(SpokePin {
                        rev: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                        via: Some("submodule vendor/app".to_owned()),
                    }),
                ),
                spoke(
                    "deploy-b",
                    "0.0.0.0:9000",
                    Some(SpokePin {
                        rev: "v2.1.0".to_owned(),
                        via: None,
                    }),
                ),
                spoke("deploy-c", "0.0.0.0:9100", None),
            ],
            true,
        )
    }

    /// #504: the matrix is the side-by-side view, so under `--pinned` each column
    /// answers a question about a *different* hub version. Reporting the columns
    /// without reporting their revs would present three incomparable measurements
    /// as one comparison.
    #[test]
    fn a_pinned_matrix_says_which_hub_version_each_spoke_was_measured_against() {
        let m = pinned_matrix();
        let text = render_text(&m);

        assert!(text.contains("2 of 3 pinned one"), "{text}");
        // A sha is abbreviated, a tag is not — the same rule the infer report uses,
        // which is why `short_rev` is shared rather than copied.
        assert!(
            text.contains("deploy-a @ 0123456789 (via submodule vendor/app)"),
            "{text}"
        );
        assert!(text.contains("deploy-b @ v2.1.0"), "{text}");
        // Named, not omitted: absence would read as "not in the matrix" (#505).
        assert!(text.contains("deploy-c @ HEAD (no pin detected)"), "{text}");
    }

    /// The HTML puts it in the column header, because that is where a reader
    /// compares two cells and would otherwise be comparing two hub versions.
    #[test]
    fn the_html_column_headers_carry_each_spokes_pin() {
        let html = render_html(&pinned_matrix());
        assert!(html.contains("@ 0123456789"), "{html}");
        assert!(
            html.contains("submodule vendor/app"),
            "sha's origin is the title attr"
        );
        assert!(html.contains("@ v2.1.0"), "{html}");
        assert!(
            html.contains("pin unpinned"),
            "the unpinned spoke is marked, not blank"
        );
    }

    /// An inert `--pinned` must not render byte-identically to a plain run: "we
    /// asked and none of them pinned anything" is a different claim from "we did
    /// not ask" (#505), and it is the claim that tells a user their workspace has
    /// no detectable pins rather than that the flag did nothing.
    #[test]
    fn a_pinned_run_that_found_no_pins_still_says_it_asked() {
        let hub_values = BTreeMap::from([("serve.addr".to_owned(), "127.0.0.1:8017".to_owned())]);
        let unpinned = || SpokeInput {
            name: "deploy".to_owned(),
            matches: vec![MatchInput {
                hub_key: "serve.addr".to_owned(),
                file: "config.toml".to_owned(),
                spoke_key: "SERVE_ADDR".to_owned(),
                spoke_value: "0.0.0.0:8443".to_owned(),
                confidence: 0.9,
                provenance: Provenance::Inferred,
            }],
            orphans: vec![],
            pin: None,
        };
        let asked = render_text(&build("app", &hub_values, vec![unpinned()], true));
        let not_asked = render_text(&build("app", &hub_values, vec![unpinned()], false));

        assert_ne!(asked, not_asked, "an inert --pinned must not be invisible");
        assert!(asked.contains("0 of 1 pinned one"), "{asked}");
        assert!(!not_asked.contains("pinned"), "{not_asked}");
    }

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
                    file: "config.toml".to_owned(),
                    spoke_key: "SERVE_ADDR".to_owned(),
                    spoke_value: "0.0.0.0:8443".to_owned(), // differs → override
                    confidence: 0.9,
                    provenance: Provenance::Inferred,
                },
                MatchInput {
                    hub_key: "serve.tools".to_owned(),
                    file: "config.toml".to_owned(),
                    spoke_key: "SERVE_TOOLS".to_owned(),
                    spoke_value: "true".to_owned(), // same → redundant
                    confidence: 0.98,
                    provenance: Provenance::Inferred,
                },
            ],
            orphans: vec![("MAX_CONNECTIONS".to_owned(), "512".to_owned())],
            pin: None,
        }];
        build("app", &hub_values, spokes, false)
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
    fn a_redundant_restatement_never_hides_a_real_override() {
        // The same spoke sets serve.addr in two files — one matching the hub
        // (redundant), one differing (a real override). The override must win
        // regardless of the order they're fed in.
        let hub_values = BTreeMap::from([("serve.addr".to_owned(), "127.0.0.1:8017".to_owned())]);
        let same = || MatchInput {
            hub_key: "serve.addr".to_owned(),
            file: "config.toml".to_owned(),
            spoke_key: "serve.addr".to_owned(),
            spoke_value: "127.0.0.1:8017".to_owned(),
            confidence: 0.98,
            provenance: Provenance::Inferred,
        };
        let over = || MatchInput {
            hub_key: "serve.addr".to_owned(),
            file: "config.toml".to_owned(),
            spoke_key: "SERVE_ADDR".to_owned(),
            spoke_value: "0.0.0.0:8443".to_owned(),
            confidence: 0.9,
            provenance: Provenance::Inferred,
        };
        for matches in [vec![same(), over()], vec![over(), same()]] {
            let m = build(
                "app",
                &hub_values,
                vec![SpokeInput {
                    name: "deploy".to_owned(),
                    matches,
                    orphans: vec![],
                    pin: None,
                }],
                false,
            );
            let cell = &m.rows[0].cells["deploy"];
            assert!(
                cell.differs,
                "override must survive a redundant restatement"
            );
            assert_eq!(cell.value, "0.0.0.0:8443");
        }
    }

    #[test]
    fn build_carries_real_per_cell_provenance() {
        // An authored override and an inferred one, side by side: each cell must
        // carry its own provenance verbatim — not a confidence-derived guess.
        let hub_values = BTreeMap::from([
            ("serve.addr".to_owned(), "127.0.0.1:8017".to_owned()),
            ("serve.tools".to_owned(), "true".to_owned()),
        ]);
        let m = build(
            "app",
            &hub_values,
            vec![SpokeInput {
                name: "deploy".to_owned(),
                matches: vec![
                    MatchInput {
                        hub_key: "serve.addr".to_owned(),
                        file: "config.toml".to_owned(),
                        spoke_key: "SERVE_ADDR".to_owned(),
                        spoke_value: "0.0.0.0:8443".to_owned(),
                        confidence: 0.0, // authored links carry no score
                        provenance: Provenance::Authored,
                    },
                    MatchInput {
                        hub_key: "serve.tools".to_owned(),
                        file: "config.toml".to_owned(),
                        spoke_key: "SERVE_TOOLS".to_owned(),
                        spoke_value: "false".to_owned(),
                        confidence: 0.9,
                        provenance: Provenance::Inferred,
                    },
                ],
                orphans: vec![],
                pin: None,
            }],
            false,
        );
        let addr = m.rows.iter().find(|r| r.hub_key == "serve.addr").unwrap();
        assert_eq!(addr.cells["deploy"].provenance, Provenance::Authored);
        let tools = m.rows.iter().find(|r| r.hub_key == "serve.tools").unwrap();
        assert_eq!(tools.cells["deploy"].provenance, Provenance::Inferred);
        // It serializes to the stable lowercase token the UI colours by.
        let json = serde_json::to_value(&m).unwrap();
        let addr_cell = &json["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["hub_key"] == "serve.addr")
            .unwrap()["cells"]["deploy"];
        assert_eq!(addr_cell["provenance"], "authored");
    }

    #[test]
    fn build_carries_the_hub_source_file_onto_each_row() {
        // Each row records the file its hub key was read from, so a client (the
        // explorer's "hide tooling config" toggle) and the CLI can classify the row
        // as app vs tooling config. A `Cargo.toml`-sourced key rides through the
        // build unchanged (the filter is opt-in — build never drops it) and its file
        // serialises verbatim, ready for `is_tooling_config_path`.
        let hub_values = BTreeMap::from([
            ("serve.addr".to_owned(), "127.0.0.1:8017".to_owned()),
            ("package.name".to_owned(), "roteiro".to_owned()),
        ]);
        let m = build(
            "app",
            &hub_values,
            vec![SpokeInput {
                name: "deploy".to_owned(),
                matches: vec![
                    MatchInput {
                        hub_key: "serve.addr".to_owned(),
                        file: "config.toml".to_owned(),
                        spoke_key: "SERVE_ADDR".to_owned(),
                        spoke_value: "0.0.0.0:8443".to_owned(),
                        confidence: 0.9,
                        provenance: Provenance::Inferred,
                    },
                    MatchInput {
                        hub_key: "package.name".to_owned(),
                        file: "Cargo.toml".to_owned(),
                        spoke_key: "PACKAGE_NAME".to_owned(),
                        spoke_value: "deploy".to_owned(),
                        confidence: 0.9,
                        provenance: Provenance::Inferred,
                    },
                ],
                orphans: vec![],
                pin: None,
            }],
            false,
        );
        let app = m.rows.iter().find(|r| r.hub_key == "serve.addr").unwrap();
        assert_eq!(app.file, "config.toml");
        let tooling = m.rows.iter().find(|r| r.hub_key == "package.name").unwrap();
        assert_eq!(tooling.file, "Cargo.toml");
        // The per-row file serialises additively for the client/CLI to classify.
        let json = serde_json::to_value(&m).unwrap();
        let tooling_json = json["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["hub_key"] == "package.name")
            .unwrap();
        assert_eq!(tooling_json["file"], "Cargo.toml");
    }

    #[test]
    fn a_hub_key_from_two_different_files_yields_an_ambiguous_empty_row_file() {
        // A `Row` is keyed by the dotted `hub_key` alone, but the same dotted key can
        // live in more than one hub file. If matches disagree on the source file, the
        // row's file is ambiguous: `build` must clear it (not keep an arbitrary
        // first-seen file), so the opt-in tooling filter treats the row as app config
        // and never hides it on a guess. Order-independent, and a re-stated file must
        // not resurrect the cleared value.
        let hub_values = BTreeMap::from([("shared.key".to_owned(), "v".to_owned())]);
        let m = |file: &str, spoke_key: &str| MatchInput {
            hub_key: "shared.key".to_owned(),
            file: file.to_owned(),
            spoke_key: spoke_key.to_owned(),
            spoke_value: "x".to_owned(),
            confidence: 0.9,
            provenance: Provenance::Inferred,
        };
        // Two spokes resolve the same hub key to different files; a third repeats the
        // first file — the row must stay ambiguous (empty) regardless.
        for matches in [
            vec![
                m("config.toml", "A"),
                m("Cargo.toml", "B"),
                m("config.toml", "C"),
            ],
            vec![m("Cargo.toml", "B"), m("config.toml", "A")],
        ] {
            let spokes = matches
                .into_iter()
                .enumerate()
                .map(|(i, mat)| SpokeInput {
                    name: format!("spoke{i}"),
                    matches: vec![mat],
                    orphans: vec![],
                    pin: None,
                })
                .collect();
            let built = build("app", &hub_values, spokes, false);
            let row = built
                .rows
                .iter()
                .find(|r| r.hub_key == "shared.key")
                .unwrap();
            assert_eq!(
                row.file, "",
                "conflicting hub-key files must leave the row file empty (unclassifiable), not an arbitrary pick"
            );
        }

        // Sanity: a hub key seen in ONE consistent file keeps that file — ambiguity
        // only clears on a genuine conflict, so tooling rows still classify.
        let consistent = build(
            "app",
            &hub_values,
            vec![
                SpokeInput {
                    name: "a".to_owned(),
                    matches: vec![m("Cargo.toml", "A")],
                    orphans: vec![],
                    pin: None,
                },
                SpokeInput {
                    name: "b".to_owned(),
                    matches: vec![m("Cargo.toml", "B")],
                    orphans: vec![],
                    pin: None,
                },
            ],
            false,
        );
        assert_eq!(consistent.rows[0].file, "Cargo.toml");
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
        let m = build("app", &hub_values, vec![], false);
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

    #[test]
    fn a_drift_key_set_by_multiple_spokes_is_one_row_with_a_cell_per_deploy() {
        // Two deploy repos both set `dq.mode` (absent from the hub) — to *different*
        // values — and one also sets a second orphan `component`. Drift must collapse
        // to exactly one row per distinct key (not one per (deploy, key) occurrence),
        // each deploy's value carried in its own column, and distinct keys stay
        // distinct rows. This is the bug: the drift band duplicated a key set by N
        // deploys into N rows.
        let hub_values = BTreeMap::new();
        let m = build(
            "app",
            &hub_values,
            vec![
                SpokeInput {
                    name: "deploy-a".to_owned(),
                    matches: vec![],
                    orphans: vec![
                        ("dq.mode".to_owned(), "strict".to_owned()),
                        ("component".to_owned(), "ingest".to_owned()),
                    ],
                    pin: None,
                },
                SpokeInput {
                    name: "deploy-b".to_owned(),
                    matches: vec![],
                    orphans: vec![("dq.mode".to_owned(), "lax".to_owned())],
                    pin: None,
                },
            ],
            false,
        );
        // `dq.mode` is set by two deploys → ONE row, not two.
        assert_eq!(
            m.drift.iter().filter(|d| d.key == "dq.mode").count(),
            1,
            "a drift key set by 2 deploys must collapse to a single row"
        );
        // Exactly two distinct drift rows overall: `component` and `dq.mode`
        // (sorted by key, since `drift` is assembled from a BTreeMap).
        assert_eq!(m.drift.len(), 2);
        assert_eq!(m.drift[0].key, "component");
        assert_eq!(m.drift[1].key, "dq.mode");
        // Both deploys' differing values populate their own columns — no info lost.
        let mode = m.drift.iter().find(|d| d.key == "dq.mode").unwrap();
        assert_eq!(mode.cells.len(), 2);
        assert_eq!(mode.cells["deploy-a"].value, "strict");
        assert_eq!(mode.cells["deploy-b"].value, "lax");
        // The single-deploy key stays its own row with just that deploy's column.
        let comp = m.drift.iter().find(|d| d.key == "component").unwrap();
        assert_eq!(comp.cells.len(), 1);
        assert_eq!(comp.cells["deploy-a"].value, "ingest");
        // Every drifting deploy is a matrix column, so its drift value has a place
        // to render even when the deploy overrides no hub key.
        assert!(m.spokes.contains(&"deploy-a".to_owned()));
        assert!(m.spokes.contains(&"deploy-b".to_owned()));

        // The dedup survives serialization the UI consumes: one object per key with
        // a per-spoke `cells` map, exactly mirroring the override rows.
        let json = serde_json::to_value(&m).unwrap();
        let drift = json["drift"].as_array().unwrap();
        assert_eq!(drift.len(), 2);
        assert_eq!(drift[1]["key"], "dq.mode");
        assert_eq!(drift[1]["cells"]["deploy-a"]["value"], "strict");
        assert_eq!(drift[1]["cells"]["deploy-b"]["value"], "lax");
    }

    #[test]
    fn a_repeated_orphan_key_within_one_spoke_collapses_to_one_cell() {
        // A single deploy lists the same orphan key twice (e.g. read from two files),
        // once blank and once with a value. It must stay one row / one cell, keeping
        // the real value rather than a blank restatement — order-independent.
        let hub_values = BTreeMap::new();
        for orphans in [
            vec![
                ("component".to_owned(), String::new()),
                ("component".to_owned(), "ingest".to_owned()),
            ],
            vec![
                ("component".to_owned(), "ingest".to_owned()),
                ("component".to_owned(), String::new()),
            ],
        ] {
            let m = build(
                "app",
                &hub_values,
                vec![SpokeInput {
                    name: "deploy".to_owned(),
                    matches: vec![],
                    orphans,
                    pin: None,
                }],
                false,
            );
            assert_eq!(m.drift.len(), 1);
            assert_eq!(m.drift[0].cells.len(), 1, "one deploy → one cell");
            let cell = &m.drift[0].cells["deploy"];
            assert_eq!(cell.value, "ingest");
            assert!(!cell.conflict, "a blank restatement is not a conflict");
        }
    }

    #[test]
    fn a_spoke_setting_one_drift_key_two_ways_is_a_deterministic_conflict_cell() {
        // A single deploy sets the same orphan key to two DIFFERENT non-empty values
        // (e.g. two files in that repo disagree). Rather than an order-dependent
        // silent overwrite that drops one value, the cell must be a deterministic
        // conflict carrying BOTH values — identical regardless of orphan order.
        let hub_values = BTreeMap::new();
        for orphans in [
            vec![
                ("dq.mode".to_owned(), "strict".to_owned()),
                ("dq.mode".to_owned(), "lax".to_owned()),
            ],
            vec![
                ("dq.mode".to_owned(), "lax".to_owned()),
                ("dq.mode".to_owned(), "strict".to_owned()),
            ],
        ] {
            let m = build(
                "app",
                &hub_values,
                vec![SpokeInput {
                    name: "deploy".to_owned(),
                    matches: vec![],
                    orphans,
                    pin: None,
                }],
                false,
            );
            assert_eq!(m.drift.len(), 1);
            let cell = &m.drift[0].cells["deploy"];
            assert!(cell.conflict, "two differing non-empty values → a conflict");
            // Deterministic sorted join — no value dropped, order-independent.
            assert_eq!(cell.value, "lax | strict");
            // The flag serialises for the explorer to render the conflict.
            let json = serde_json::to_value(&m).unwrap();
            assert_eq!(json["drift"][0]["cells"]["deploy"]["conflict"], true);
            assert_eq!(json["drift"][0]["cells"]["deploy"]["value"], "lax | strict");
        }

        // A consistent restatement of the SAME value is not a conflict — the common
        // single-value cell is unchanged.
        let m = build(
            "app",
            &hub_values,
            vec![SpokeInput {
                name: "deploy".to_owned(),
                matches: vec![],
                orphans: vec![
                    ("dq.mode".to_owned(), "strict".to_owned()),
                    ("dq.mode".to_owned(), "strict".to_owned()),
                ],
                pin: None,
            }],
            false,
        );
        let cell = &m.drift[0].cells["deploy"];
        assert!(!cell.conflict);
        assert_eq!(cell.value, "strict");
    }
}

//! The **one** place a tool's class is written, and the report that stands in for
//! a class an operator did not load.
//!
//! # Why classes at all
//!
//! Every advertised tool costs tokens on **every turn**, whether or not the
//! session could ever reach it. Measured on the surface this module classifies,
//! `security` and `sandbox` are 51% of the description mass and are the two
//! groups a code-navigation session never touches — so a session that only wants
//! to read code pays roughly half its tool budget to advertise `sandbox_clear`,
//! which is also the only tool on the surface that changes anything.
//!
//! [`crate::mcp::restrict`] already resolves an operator's list of tool *names*
//! (issue #584). A class is that same mechanism with a name a person can actually
//! type: `--tools query,quality` rather than ten names that go stale the moment a
//! tool is added. Narrowing stays opt-in — the default advertises every class, so
//! a server nobody configured behaves exactly as it did.
//!
//! # Why a class must stay discoverable
//!
//! A withheld class is invisible from the client side, and an invisible tool and
//! an impossible one look identical to a model: asked about analyzer findings on
//! a `query`-only server it would answer *"Roteiro cannot do that"*, which is
//! false. [`CLASS_INDEX_TOOL`] is the fix and is why it is never withheld — it
//! names every class, says which are loaded here, and costs a fraction of the
//! prose it stands in for. The answer becomes "not loaded in this session, and
//! here is the flag that loads it".
//!
//! # The taxonomy is total
//!
//! Every tool belongs to exactly one class, and `every_tool_has_exactly_one_class`
//! in [`crate::mcp`] fails if a tool is added to the surface and not to a class —
//! which would otherwise make it unreachable through the class aliases while
//! remaining reachable by name, a surface with two disagreeing halves.

/// The tool that names the classes, and the one tool belonging to none of them.
///
/// It is not in [`CLASSES`] on purpose: it is the index, not an entry, and a
/// restriction that could withhold it would remove the only way a client learns
/// what was withheld.
pub const CLASS_INDEX_TOOL: &str = "list_tool_classes";

/// Each class and the tools it names, in the order a reader meets them.
///
/// Alphabetical within a class so a diff to this table is readable, and the
/// classes themselves ordered by what a session reaches for first.
pub const CLASSES: [(&str, &[&str]); 4] = [
    (
        "query",
        &[
            "context",
            "explain",
            "list_kind",
            "list_projects",
            "path",
            "search",
        ],
    ),
    (
        "quality",
        &[
            "check",
            "config_secrets",
            "coupling",
            "debt",
            "debt_density",
        ],
    ),
    ("security", &["security_list", "security_status"]),
    ("sandbox", &["sandbox_clear", "sandbox_status"]),
];

/// The tools `class` names, or `None` for a word that is not a class.
#[must_use]
pub fn tools_in(class: &str) -> Option<&'static [&'static str]> {
    CLASSES
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, tools)| *tools)
}

/// The class `tool` belongs to, or `None` for [`CLASS_INDEX_TOOL`] and for a name
/// this taxonomy does not carry.
#[must_use]
pub fn class_of(tool: &str) -> Option<&'static str> {
    CLASSES
        .iter()
        .find(|(_, tools)| tools.contains(&tool))
        .map(|(name, _)| *name)
}

/// Every class name, for an error message that has to list them.
#[must_use]
pub fn class_names() -> Vec<&'static str> {
    CLASSES.iter().map(|(name, _)| *name).collect()
}

/// What a single tool's presence is, from a caller's point of view.
///
/// Three states rather than a boolean because the **remedies differ**, and a
/// client told only "absent" would guess. `withheld` is an operator's `--tools`
/// and is undone by widening it; `unavailable` is a tool this build or this
/// surface does not carry at all, and no flag reaches it.
fn tool_state(in_build: bool, advertised: bool) -> &'static str {
    match (in_build, advertised) {
        (false, _) => "unavailable",
        (true, true) => "loaded",
        (true, false) => "withheld",
    }
}

/// The document [`CLASS_INDEX_TOOL`] returns: every class, every tool in it, and
/// what each one's presence is here.
///
/// Both predicates are asked per tool, and both are needed. `in_build` answers
/// whether this build or surface carries the tool at all; `advertised` answers
/// whether the operator's selection kept it. Collapsing them into one would make
/// a feature gate and a `--tools` flag indistinguishable in the reply, and they
/// have different remedies — see [`tool_state`].
///
/// Shared by both surfaces rather than written twice: a model that reads a
/// different class table over MCP than over served chat has been told two
/// different things about one server, which is the drift [`crate::tool_text`]
/// exists to prevent for descriptions.
#[must_use]
pub fn report(
    in_build: impl Fn(&str) -> bool,
    advertised: impl Fn(&str) -> bool,
) -> serde_json::Value {
    let classes: Vec<serde_json::Value> = CLASSES
        .iter()
        .map(|(class, tools)| {
            let rows: Vec<serde_json::Value> = tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "tool": tool,
                        "state": tool_state(in_build(tool), advertised(tool)),
                    })
                })
                .collect();
            let present = tools.iter().filter(|t| in_build(t)).count();
            let loaded = tools
                .iter()
                .filter(|t| in_build(t) && advertised(t))
                .count();
            let state = match (present, loaded) {
                (0, _) => "unavailable",
                (_, 0) => "not-loaded-here",
                (p, l) if p == l => "loaded",
                _ => "partly-loaded",
            };
            serde_json::json!({ "class": class, "state": state, "tools": rows })
        })
        .collect();
    serde_json::json!({
        "classes": classes,
        "note": "A class is `not-loaded-here` when this server was started without it \
                 (`roteiro serve --tools query,quality`, or `[mcp] tools` in \
                 `roteiro.toml`). That is a startup choice made to keep unused tool \
                 descriptions out of every turn's prompt — it is NOT a capability \
                 Roteiro lacks. Say so in those words and name the class, so the user \
                 can restart the server with it, rather than reporting that Roteiro \
                 cannot answer the question. A tool marked `unavailable` is a \
                 different case: this build or this surface does not carry it, and no \
                 startup flag reaches it.",
    })
}

#[cfg(test)]
mod tests {
    use super::{CLASS_INDEX_TOOL, CLASSES, class_names, class_of, report, tools_in};
    use std::collections::BTreeSet;

    /// No tool may sit in two classes.
    ///
    /// [`class_of`] returns the first match, so a duplicate would silently pick a
    /// winner and make `--tools <other class>` quietly not select it.
    #[test]
    fn no_tool_belongs_to_two_classes() {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for (class, tools) in CLASSES {
            for tool in tools {
                assert!(
                    seen.insert(tool),
                    "`{tool}` appears twice; the second time in `{class}`"
                );
            }
        }
        assert!(
            !seen.contains(CLASS_INDEX_TOOL),
            "the class index belongs to no class — a restriction able to withhold it \
             would remove the only way a client learns what was withheld"
        );
    }

    /// The class names are distinct and none of them is a tool name, because both
    /// are accepted in the same `--tools` list and a collision would make one
    /// unreachable.
    #[test]
    fn a_class_name_is_never_also_a_tool_name() {
        let names: BTreeSet<&str> = class_names().into_iter().collect();
        assert_eq!(names.len(), CLASSES.len(), "duplicate class name");
        for class in class_names() {
            assert!(
                class_of(class).is_none(),
                "`{class}` is both a class and a tool"
            );
            assert!(tools_in(class).is_some());
        }
        assert!(tools_in("query").is_some_and(|t| t.contains(&"search")));
        assert!(tools_in("nope").is_none());
    }

    /// The report distinguishes an operator's withholding from a missing build.
    ///
    /// Both look like "no such tool" from the client side and only one of them has
    /// a remedy the operator can apply, so a report that collapsed them would send
    /// a user to change a flag that cannot help.
    #[test]
    fn the_report_separates_a_withheld_tool_from_an_absent_one() {
        // `list_kind` stands for the not-on-this-surface case, `search` for the
        // loaded one, and everything else for withheld.
        let doc = report(|t| t != "list_kind", |t| t == "search");
        let classes = doc["classes"].as_array().expect("classes array");
        let query = classes
            .iter()
            .find(|c| c["class"] == "query")
            .expect("query class");
        assert_eq!(query["state"], "partly-loaded", "{doc}");
        let state_of = |name: &str| {
            query["tools"]
                .as_array()
                .expect("tools array")
                .iter()
                .find(|t| t["tool"] == name)
                .map(|t| t["state"].clone())
                .expect("tool row")
        };
        assert_eq!(state_of("search"), "loaded", "{doc}");
        assert_eq!(state_of("explain"), "withheld", "{doc}");
        assert_eq!(state_of("list_kind"), "unavailable", "{doc}");

        let security = classes
            .iter()
            .find(|c| c["class"] == "security")
            .expect("security class");
        assert_eq!(security["state"], "not-loaded-here", "{doc}");
    }

    /// A fully loaded surface reports every class loaded — the default, and the
    /// state a server nobody configured is in.
    #[test]
    fn an_unrestricted_surface_reports_every_class_loaded() {
        let doc = report(|_| true, |_| true);
        for class in doc["classes"].as_array().expect("classes array") {
            assert_eq!(class["state"], "loaded", "{doc}");
        }
    }
}

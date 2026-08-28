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

/// A tool advertised on this server.
const LOADED: &str = "loaded";
/// A tool this build carries that the operator's selection did not keep.
const WITHHELD: &str = "withheld";
/// A tool this build or surface does not carry at all.
const UNAVAILABLE: &str = "unavailable";
/// A class with no tool advertised here.
const NOT_LOADED_HERE: &str = "not-loaded-here";
/// A class advertised in part.
const PARTLY_LOADED: &str = "partly-loaded";

/// Every state this module can emit, each with the one line that defines it.
///
/// # Why the note is generated from this rather than written beside it
///
/// The note exists to stop a model reporting a withheld tool as a capability
/// Roteiro does not have, and it can only do that if a model reading **only the
/// JSON** can tell the states apart. A hand-written note did not hold that: it
/// explained `not-loaded-here` and `unavailable` while the payload also emitted
/// `loaded`, `withheld` and `partly-loaded` with nothing defining them — and
/// `withheld` is precisely the case the note is for.
///
/// Building the note from this table makes *no observable state is undefined*
/// true by construction rather than by remembering. The states are consts rather
/// than literals at each match arm for the same reason: the value in the payload
/// and the entry that explains it are one string, so they cannot come apart.
///
/// Definitions are terse deliberately. This travels back on every call, and a
/// feature whose point is spending fewer tokens should not restate itself.
const STATE_GLOSSARY: [(&str, &str); 5] = [
    (LOADED, "advertised here"),
    // No definition may contain a semicolon: `note` joins the entries with one, so
    // an internal semicolon reads as a further state with no name.
    (
        WITHHELD,
        "this build has the tool but the operator's `--tools` did not select its class",
    ),
    (NOT_LOADED_HERE, "no tool of the class is advertised here"),
    (
        PARTLY_LOADED,
        "some of the class is advertised, some withheld",
    ),
    (
        UNAVAILABLE,
        "this build or surface does not carry it at all, and no startup flag reaches it",
    ),
];

/// What a caller should *do* about those states — advice rather than definition,
/// so it is stated once instead of repeated per entry.
const STATE_GUIDANCE: &str = "`withheld`, `not-loaded-here` and `partly-loaded` are STARTUP CHOICES made to keep unused tool descriptions out of every turn's prompt (`roteiro serve --tools query,quality`, or `[mcp] tools` in `roteiro.toml`) — they are NOT capabilities Roteiro lacks. Name the class so the user can restart the server with it, rather than reporting that Roteiro cannot answer the question.";

/// The rendered definition of `state` as it appears in a [`note`].
///
/// Shared with the test so "is this state defined" asks about the **definition**
/// and not about the word appearing anywhere. [`STATE_GUIDANCE`] names three
/// states in passing, so a bare substring check would call those defined however
/// the glossary changed underneath it.
fn definition_prefix(state: &str) -> String {
    format!("`{state}` = ")
}

/// The `note` a [`report`] carries: every observable state defined, then what to
/// do about them.
fn note() -> String {
    let defined: Vec<String> = STATE_GLOSSARY
        .iter()
        .map(|(name, meaning)| format!("{}{meaning}", definition_prefix(name)))
        .collect();
    format!("`state` values: {}. {STATE_GUIDANCE}", defined.join("; "))
}

/// What a single tool's presence is, from a caller's point of view.
///
/// Three states rather than a boolean because the **remedies differ**, and a
/// client told only "absent" would guess. [`WITHHELD`] is an operator's `--tools`
/// and is undone by widening it; [`UNAVAILABLE`] is a tool this build or this
/// surface does not carry at all, and no flag reaches it.
fn tool_state(in_build: bool, advertised: bool) -> &'static str {
    match (in_build, advertised) {
        (false, _) => UNAVAILABLE,
        (true, true) => LOADED,
        (true, false) => WITHHELD,
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
                (0, _) => UNAVAILABLE,
                (_, 0) => NOT_LOADED_HERE,
                (p, l) if p == l => LOADED,
                _ => PARTLY_LOADED,
            };
            serde_json::json!({ "class": class, "state": state, "tools": rows })
        })
        .collect();
    serde_json::json!({
        "classes": classes,
        "note": note(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CLASS_INDEX_TOOL, CLASSES, STATE_GLOSSARY, class_names, class_of, definition_prefix, note,
        report, tools_in,
    };
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

    /// Every `state` string a caller can observe is **defined in the note**.
    ///
    /// The note's whole job is to stop a model reporting a withheld tool as a
    /// capability Roteiro does not have, and it can only do that if a model
    /// reading nothing but this JSON can tell the states apart. It could not: it
    /// explained two of the five, and the three it omitted included `withheld` —
    /// exactly the case it exists for.
    ///
    /// Both sides are **derived**, not listed. The emitted set is collected by
    /// walking the real payload for every `state` key, over scenarios chosen to
    /// produce each one; the defined set is [`STATE_GLOSSARY`], which the note is
    /// built from. A test that grepped for today's words would pass while a sixth
    /// state shipped undefined — the same doc-describes-code-inaccurately shape
    /// this module already had once.
    ///
    /// The membership check asks for the rendered **definition**
    /// ([`definition_prefix`]), not for the word: [`STATE_GUIDANCE`] names three
    /// states in passing, so a bare substring test would call those defined no
    /// matter what the glossary did.
    #[test]
    fn every_state_the_report_emits_is_defined_in_its_note() {
        use std::collections::BTreeSet;

        /// Every `"state"` value anywhere in the document, at any depth, so the
        /// collection does not depend on the payload's current shape.
        fn states_in(value: &serde_json::Value, found: &mut BTreeSet<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        if key == "state"
                            && let Some(state) = child.as_str()
                        {
                            found.insert(state.to_owned());
                        }
                        states_in(child, found);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        states_in(item, found);
                    }
                }
                _ => {}
            }
        }

        // Chosen to drive every arm of both state machines. The last one is not
        // redundant with the one above it, and the set equality below is what
        // proved that: a whole-class selection only ever yields `loaded` or
        // `not-loaded-here`, so without a *partially* selected class nothing
        // emitted `partly-loaded` and it would have shipped undefined. That is the
        // case for deriving the emitted set instead of listing it.
        /// One predicate pair to drive [`report`] with, and a label for the
        /// failure message. Named because the tuple is otherwise too dense to
        /// read — and clippy says so.
        type Scenario = (&'static str, fn(&str) -> bool, fn(&str) -> bool);

        let scenarios: [Scenario; 5] = [
            ("nothing carried", |_| false, |_| false),
            ("all carried, none advertised", |_| true, |_| false),
            ("all carried, all advertised", |_| true, |_| true),
            (
                "all carried, one class advertised",
                |_| true,
                |name| class_of(name).is_some_and(|c| c == "query"),
            ),
            (
                "all carried, one tool of each class advertised",
                |_| true,
                |name| {
                    class_of(name)
                        .and_then(tools_in)
                        .and_then(<[&str]>::first)
                        .is_some_and(|first| *first == name)
                },
            ),
        ];

        let mut emitted: BTreeSet<String> = BTreeSet::new();
        for (label, in_build, advertised) in scenarios {
            let doc = report(in_build, advertised);
            let mut here = BTreeSet::new();
            states_in(&doc, &mut here);
            assert!(!here.is_empty(), "`{label}` produced no state at all");
            emitted.extend(here);
        }

        let note = note();
        for state in &emitted {
            assert!(
                note.contains(&definition_prefix(state)),
                "the report can emit `{state}` and the note never defines it. A client \
                 that sees only this JSON cannot tell it from a capability Roteiro \
                 lacks, which is the one thing the note exists to prevent. Add it to \
                 `STATE_GLOSSARY`",
            );
        }

        // The glossary must not accumulate entries for states nothing emits
        // either: an unreachable definition is prompt tokens spent on a value no
        // client will ever see.
        let defined: BTreeSet<String> = STATE_GLOSSARY
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        assert_eq!(
            emitted, defined,
            "the states the report emits and the states the glossary defines must be \
             the same set — left is emitted, right is defined",
        );

        // The glossary renders into one sentence with `; ` between entries, so a
        // definition carrying its own semicolon reads as an extra, nameless state.
        for (name, meaning) in STATE_GLOSSARY {
            assert!(
                !meaning.contains(';'),
                "`{name}`'s definition contains the separator `note` joins with, so it \
                 reads as two entries: {meaning:?}",
            );
        }
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

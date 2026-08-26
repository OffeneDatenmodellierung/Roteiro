//! The **project-level shape** of a workspace: who depends on whom, derived once
//! and read by everything that needs to know.
//!
//! # Why this is here rather than in a caller
//!
//! Issue #623 asked for two things. The first — that a project can be a spoke of
//! one project and the hub of others — landed in the served topology view. The
//! second did not: *lift the hub rule into one shared home, because there are
//! already two and pins would add a third.*
//!
//! That second half is this module, and it was not optional for long. The
//! consolidated rule lived in `roteiro`'s `graph_api`, which is
//! `#[cfg(feature = "explorer")]`, while the workspace **vault** renderer is not
//! gated at all. So the first caller outside the web API — version pins in the
//! shareable manifest (#442) — could not legally call the rule it needed, and its
//! only alternatives were to write a third one or to gate a Markdown export
//! behind a web-API feature.
//!
//! Living in `rto-graph` puts it below every caller: the explorer's JSON API, the
//! vault renderer, and the `links` views all depend on this crate unconditionally,
//! which is the same argument [`crate::slugify`] and [`crate::markdown_dialect`]
//! already make for themselves.
//!
//! # What it is built from, and what it is deliberately not built from
//!
//! **Persisted external-ref edges only** — those declared as authored `[[links]]`,
//! and those a previous `links --write` wrote to the store. Not the merged link
//! list a topology view renders: that also
//! carries the correspondences inferred *live* against the hub, which are a
//! config-key **matching heuristic**, not a declared dependency. Deriving the shape
//! from those would make every project a child of the hub by construction, and in a
//! chain (`infra → chart → app`) would invent a `chart ↔ app` cycle out of a name
//! match.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::links::{EXTERNAL_REF_KIND, external_ref_target};
use crate::model::{Node, NodeKind};
use crate::store::{Store, StoreError};
use crate::workspace::{Workspace, WorkspaceError, parse_qualified};

/// Where a project sits in the workspace hierarchy, from its own in/out degree.
///
/// Four values, replacing the `hub`/`spoke` pair that could not describe a project
/// which is both — see #623. A cycle has no root: every project in it reports
/// [`Self::Intermediate`], which is a truthful report of a workspace that declares
/// one rather than an error. Nothing here recurses, so a cycle cannot hang a
/// caller.
///
/// `#[non_exhaustive]` because this is a published crate and the set is a
/// description of shapes we have met, not a proof that no other exists — a
/// workspace form nobody has modelled yet would add a variant, and that must not
/// be a breaking change (#431).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectRole {
    /// Depends on nothing hosted, and something depends on it — the end of every
    /// chain; the application a deployment tree ultimately deploys.
    Root,
    /// A **sub-hub**: depends on something *and* is depended upon. The case a
    /// two-valued label had no room for.
    Intermediate,
    /// Depends on something hosted, and nothing depends on it. An ordinary spoke,
    /// and still the common case.
    Leaf,
    /// Neither. A project in the workspace with no declared cross-repo links yet,
    /// which a two-valued label reported as a spoke of a hub it never named.
    Isolated,
}

impl ProjectRole {
    /// The wire spelling, as the served topology publishes it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Intermediate => "intermediate",
            Self::Leaf => "leaf",
            Self::Isolated => "isolated",
        }
    }
}

/// A workspace's project-level dependency shape.
#[derive(Debug, Clone, Default)]
pub struct ProjectGraph {
    /// For each project, the hosted projects it points **into** — the hubs it
    /// depends on. A project is never its own parent, so a self-reference (a repo
    /// whose link targets its own project name) is dropped.
    parents: BTreeMap<String, BTreeSet<String>>,
    /// For each project, how many external-ref **edges** point into it. Counts
    /// edges, not distinct projects: five keys in one repo referencing the hub are
    /// five, which is what has always decided the hub tiebreak.
    inbound_edges: BTreeMap<String, usize>,
    /// For each project, how many **other projects** name it as a parent.
    ///
    /// Not the same number as [`Self::inbound_edges`] and not derivable from it:
    /// that counts edges and includes self-references, while this counts distinct
    /// dependent projects and excludes them. Only this answers "is anything
    /// downstream of me".
    children: BTreeMap<String, usize>,
    /// Whether **any** project carries a persisted external-ref edge at all — set
    /// before the hosted-target filter, so a workspace whose links all point at
    /// unhosted repos still reports `true`.
    ///
    /// That distinction is why it is a flag rather than `!parents.is_empty()`:
    /// "nothing has been linked yet" falls back to inference, while "links exist
    /// but dangle" keeps a `None` hub, and both leave `parents` empty.
    has_any_external_refs: bool,
}

impl ProjectGraph {
    /// The hosted projects `name` depends on, in name order. Empty for a root or an
    /// isolated project.
    #[must_use]
    pub fn parents_of(&self, name: &str) -> &BTreeSet<String> {
        static NONE: std::sync::LazyLock<BTreeSet<String>> =
            std::sync::LazyLock::new(BTreeSet::new);
        self.parents.get(name).unwrap_or(&NONE)
    }

    /// How many external-ref **edges** point into `name`.
    ///
    /// Exposed because it is what [`Self::busiest_hub`] reduces, and because it is
    /// the number [`Self::children_of`] is most likely to be confused with: this
    /// counts edges and includes self-references, that counts distinct dependent
    /// projects and excludes them. A spoke referencing the hub from twenty config
    /// keys contributes twenty here and one there.
    #[must_use]
    pub fn inbound_edges_of(&self, name: &str) -> usize {
        self.inbound_edges.get(name).copied().unwrap_or(0)
    }

    /// How many hosted projects depend on `name`.
    ///
    /// A lookup rather than a scan: it is called once per project, and scanning
    /// every `parents` set each time made role assignment O(n²) in the number of
    /// projects for an answer already known while the map was built.
    #[must_use]
    pub fn children_of(&self, name: &str) -> usize {
        self.children.get(name).copied().unwrap_or(0)
    }

    /// Where `name` sits in the hierarchy, from its own in/out degree.
    #[must_use]
    pub fn role_of(&self, name: &str) -> ProjectRole {
        let has_parents = !self.parents_of(name).is_empty();
        match (has_parents, self.children_of(name) > 0) {
            (false, true) => ProjectRole::Root,
            (true, true) => ProjectRole::Intermediate,
            (true, false) => ProjectRole::Leaf,
            (false, false) => ProjectRole::Isolated,
        }
    }

    /// Whether any project carries a persisted external-ref edge at all.
    #[must_use]
    pub fn has_any_external_refs(&self) -> bool {
        self.has_any_external_refs
    }

    /// The **hosted** project most external-ref edges point into, or `None` when
    /// nothing references a hosted project (a single-repo or unlinked workspace).
    ///
    /// Note what this is *not*: in a snowflake it names the busiest node, which
    /// need not be the chain's root. `infra1`/`infra2` → `chart` → `app` makes
    /// `chart` the hub on two inbound edges while `app` is what everything
    /// ultimately depends on. That is the right answer for this function's one job
    /// — picking the config-key baseline the override matrix pivots on — and the
    /// wrong answer for "where does the chain end", which is what [`Self::role_of`]
    /// reports instead. In a star the two coincided, which is why one field used to
    /// serve both.
    #[must_use]
    pub fn busiest_hub(&self) -> Option<String> {
        self.inbound_edges
            .iter()
            .max_by_key(|(_, count)| **count)
            .map(|(p, _)| p.clone())
    }
}

/// Build the [`ProjectGraph`] by walking every hosted project's persisted external
/// refs once.
///
/// `names` is the set of **hosted** projects: a ref naming a project outside it is
/// counted towards [`ProjectGraph::has_any_external_refs`] but contributes no edge,
/// so the shape never contains a project the workspace cannot read.
///
/// # Errors
///
/// Propagates any [`WorkspaceError`] from selecting a member, and any
/// [`StoreError`] from reading its store — the latter converted, since a store
/// that will not open must not be reported as a project with no dependencies.
/// Swallowing it would silently move the hub and change every role.
pub fn project_graph(ws: &Workspace, names: &[String]) -> Result<ProjectGraph, WorkspaceError> {
    let hosted: HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut graph = ProjectGraph::default();
    for name in names {
        for node in ws.with_store(Some(name), external_ref_nodes)?? {
            // Before the target filter, deliberately: a ref that names an unhosted
            // project is still a ref, and the caller distinguishing "never linked"
            // from "linked but dangling" depends on seeing it.
            graph.has_any_external_refs = true;
            let Some(qualified) = external_ref_target(&node) else {
                continue;
            };
            let Some((project, _)) = parse_qualified(&qualified) else {
                continue;
            };
            if !hosted.contains(project) {
                continue;
            }
            *graph.inbound_edges.entry(project.to_owned()).or_default() += 1;
            if project != name.as_str()
                // Only a *newly* inserted parent is a new dependent: a spoke
                // referencing the hub from twenty config keys is one child of it,
                // not twenty.
                && graph
                    .parents
                    .entry(name.clone())
                    .or_default()
                    .insert(project.to_owned())
            {
                *graph.children.entry(project.to_owned()).or_default() += 1;
            }
        }
    }
    Ok(graph)
}

/// Every external-ref placeholder node in `store` that something actually points
/// at, with `Authored` or `Inferred` provenance.
///
/// A *derived* edge never targets an external-ref placeholder, so it is excluded;
/// and a placeholder with no incoming edge is a leftover, not a dependency.
///
/// **One entry per incoming edge, not per node** — a placeholder pointed at by
/// three config keys appears three times. That is deliberate and load-bearing:
/// [`ProjectGraph::inbound_edges_of`] counts edges, which is what decides the hub
/// tiebreak, and de-duplicating here would silently turn it into a count of
/// distinct placeholders and move the hub. The *distinct* count callers usually
/// want is [`ProjectGraph::children_of`], which is derived separately.
fn external_ref_nodes(store: &Store) -> Result<Vec<Node>, StoreError> {
    let mut out = Vec::new();
    for node in store.nodes_by_kind(&NodeKind::Other(EXTERNAL_REF_KIND.to_owned()))? {
        for edge in store.edges_to(&node.key)? {
            if matches!(
                edge.provenance,
                crate::provenance::Provenance::Inferred | crate::provenance::Provenance::Authored
            ) {
                out.push(node.clone());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{ProjectRole, project_graph};
    use crate::links::{external_ref_key, external_ref_node};
    use crate::model::{Edge, EdgeKind, Node, NodeKind};
    use crate::store::Store;
    use crate::workspace::Workspace;

    /// One repo's store, holding an authored external-ref edge per target.
    ///
    /// The edge is `authored`, matching what `roteiro links --write` actually
    /// persists: a fixture pairing an authored edge with an inferred layer would be
    /// a state the product never produces.
    fn repo(own: &str, targets: &[&str]) -> Store {
        let store = Store::open_in_memory().expect("store");
        // The edge's own end must exist: the store enforces referential integrity,
        // so a fixture that only creates the placeholder is rejected rather than
        // quietly storing a half-edge.
        let src_key = format!("cfgkey:cfg.toml#{own}");
        store
            .upsert_node(&Node::new(
                src_key.clone(),
                NodeKind::Other("config_key".to_owned()),
                own.to_owned(),
            ))
            .expect("src node");
        for target in targets {
            let node = external_ref_node(target);
            store.upsert_node(&node).expect("node");
            let edge = Edge::authored(
                src_key.clone(),
                external_ref_key(target),
                EdgeKind::References,
            );
            store.insert_edge(&edge).expect("edge");
        }
        store
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The shape #623 exists for: `infra1,infra2 → chart → app`, where `chart` is a
    /// spoke of `app` and the hub of both infra repos.
    #[test]
    fn a_chain_has_a_root_a_sub_hub_and_leaves() {
        let ws = Workspace::from_stores([
            (
                "infra1".to_owned(),
                repo("a", &["chart::cfgkey:cfg.toml#c"]),
            ),
            (
                "infra2".to_owned(),
                repo("b", &["chart::cfgkey:cfg.toml#c"]),
            ),
            ("chart".to_owned(), repo("c", &["app::cfgkey:cfg.toml#d"])),
            ("app".to_owned(), repo("d", &[])),
        ]);
        let g = project_graph(&ws, &names(&["infra1", "infra2", "chart", "app"])).expect("graph");

        assert_eq!(g.role_of("app"), ProjectRole::Root, "nothing is downstream");
        assert_eq!(
            g.role_of("chart"),
            ProjectRole::Intermediate,
            "a spoke of app AND the hub of both infra repos"
        );
        assert_eq!(g.role_of("infra1"), ProjectRole::Leaf);
        assert_eq!(g.role_of("infra2"), ProjectRole::Leaf);

        assert_eq!(
            g.parents_of("chart").iter().collect::<Vec<_>>(),
            ["app"],
            "the sub-hub names its own hub"
        );
        assert!(g.parents_of("app").is_empty());

        // The busiest node is `chart`, which is NOT the root — the two questions
        // that coincide in a star and diverge in a chain.
        assert_eq!(g.busiest_hub().as_deref(), Some("chart"));
    }

    /// A project with no cross-repo links is `Isolated`, not a spoke of a hub it
    /// never named.
    #[test]
    fn a_project_with_no_links_is_isolated() {
        let ws = Workspace::from_stores([
            ("solo".to_owned(), repo("a", &[])),
            ("other".to_owned(), repo("b", &[])),
        ]);
        let g = project_graph(&ws, &names(&["solo", "other"])).expect("graph");
        assert_eq!(g.role_of("solo"), ProjectRole::Isolated);
        assert_eq!(g.busiest_hub(), None, "nothing references anything hosted");
        assert!(!g.has_any_external_refs());
    }

    /// Links that exist but name an **unhosted** project: no edge, yet the
    /// workspace is not "never linked".
    ///
    /// The distinction decides whether a caller falls back to inference or keeps a
    /// `None` hub, and both cases leave `parents` empty — so it cannot be recovered
    /// from the maps afterwards.
    #[test]
    fn a_dangling_link_is_still_a_link() {
        let ws = Workspace::from_stores([(
            "spoke".to_owned(),
            repo("a", &["ghost::cfgkey:cfg.toml#z"]),
        )]);
        let g = project_graph(&ws, &names(&["spoke"])).expect("graph");
        assert!(
            g.has_any_external_refs(),
            "the ref exists even though its target is not hosted"
        );
        assert_eq!(g.busiest_hub(), None, "nothing hosted is referenced");
        assert_eq!(g.role_of("spoke"), ProjectRole::Isolated);
    }

    /// A repo referencing the hub from many keys is **one** dependent of it, while
    /// the hub tiebreak still counts every edge. Asserted directly because both
    /// numbers are `> 0` and so produce the same role.
    #[test]
    fn many_links_from_one_repo_are_one_dependent_but_many_edges() {
        let ws = Workspace::from_stores([
            (
                "spoke".to_owned(),
                repo("a", &["hub::cfgkey:cfg.toml#x", "hub::cfgkey:cfg.toml#y"]),
            ),
            ("hub".to_owned(), repo("h", &[])),
        ]);
        let g = project_graph(&ws, &names(&["spoke", "hub"])).expect("graph");
        assert_eq!(g.children_of("hub"), 1, "one dependent project");
        assert_eq!(g.inbound_edges_of("hub"), 2, "two edges");
    }

    /// A repo whose link targets its own project is not its own parent — otherwise
    /// it would report as `Intermediate` on the strength of pointing at itself.
    #[test]
    fn a_self_reference_is_not_a_dependency() {
        let ws =
            Workspace::from_stores([("solo".to_owned(), repo("a", &["solo::cfgkey:cfg.toml#a"]))]);
        let g = project_graph(&ws, &names(&["solo"])).expect("graph");
        assert!(g.parents_of("solo").is_empty());
        assert_eq!(g.children_of("solo"), 0);
        assert_eq!(g.role_of("solo"), ProjectRole::Isolated);
        // …but the edge is still counted where edges are counted.
        assert_eq!(g.inbound_edges_of("solo"), 1);
    }
}

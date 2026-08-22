//! The **read-only `check` document** the model-facing tool surfaces return.
//!
//! `roteiro check` is a gate: it rebuilds the graph from a tree and exits
//! non-zero on drift, and the pre-commit hook reads that exit code. Over a tool
//! surface there is no exit code — there is only a document — so the one thing
//! this module exists to guarantee is that **`0 violations` and `did not run` are
//! never the same document**. See [`ToolCheck`].
//!
//! It shares its violation rule with the gate rather than restating it:
//! [`crate::authored_layer`] reads the same authored file set `build_graph` reads,
//! and [`crate::validate`] is literally the function [`crate::run`] calls. What
//! this module adds is the two preconditions a read-only caller must satisfy
//! before either is meaningful, and an honest answer when it cannot.

use std::path::Path;

use rto_graph::{GraphSource, Repo, Store};
use serde::Serialize;

use crate::check::{CheckReport, validate};
use crate::layer::authored_layer;

/// Schema tag for the tool-surface `check` document.
pub const TOOL_CHECK_SCHEMA: &str = "roteiro.check/v1";

/// The gate verdict, as a value rather than an exit code.
///
/// `NotRun` is a third state on purpose. A caller that only tests
/// `violations.is_empty()` would read a check that never ran as a clean
/// repository; making the absence of a verdict its own value means that caller
/// has to notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gate {
    /// The check ran and found no drift (`roteiro check` would exit 0).
    Pass,
    /// The check ran and found drift (`roteiro check` would exit non-zero).
    Fail,
    /// The check did **not** run. `report` is absent; see `not_run_reason`.
    NotRun,
}

/// Which graph the verdict describes, so a reader can tell what was compared.
#[derive(Debug, Clone, Serialize)]
pub struct CheckedAgainst {
    /// The tree the authored layer was read from — always `"committed"` here.
    pub source: &'static str,
    /// The `HEAD` tree id the derived graph was synced from and the authored
    /// layer was parsed from. They are equal by construction: an inequality is
    /// the `NotRun` case below, not a caveat on a verdict.
    pub tree: String,
}

/// The tool-surface `check` result.
///
/// # Why `report` is an `Option` and not an empty `CheckReport`
///
/// The whole hazard this shape addresses is that a check which could not run
/// looks exactly like a clean one once it is serialised. `CheckReport` has
/// `violations: Vec<Violation>`, and an empty vector is the *good* answer — so a
/// not-run result must not produce a `CheckReport` at all. It doesn't: `report`
/// is `None` and is skipped entirely in JSON, so a consumer reaching for
/// `violations` finds nothing rather than nothing-wrong. `gate` says the same
/// thing in one word for a consumer that reads only that.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCheck {
    /// Stable schema tag ([`TOOL_CHECK_SCHEMA`]).
    pub schema: &'static str,
    /// The verdict — `pass`, `fail`, or `not-run`. Always present.
    pub gate: Gate,
    /// The full drift report. **Absent unless the check actually ran.**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<CheckReport>,
    /// What was compared. Absent unless the check actually ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_against: Option<CheckedAgainst>,
    /// Why the check could not run. Present exactly when `gate` is `not-run`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_run_reason: Option<String>,
}

impl ToolCheck {
    /// A `not-run` document carrying `reason`. No `report`, by construction.
    fn not_run(reason: String) -> Self {
        Self {
            schema: TOOL_CHECK_SCHEMA,
            gate: Gate::NotRun,
            report: None,
            checked_against: None,
            not_run_reason: Some(reason),
        }
    }
}

/// Run `roteiro check`'s drift validation **read-only** against `store`, with the
/// authored layer read from the committed `HEAD` tree of the repository at
/// `root`.
///
/// Nothing is written: [`authored_layer`] reads git and parses text, and
/// [`validate`] only queries the store. The `authored` edges the gate would weave
/// in are discarded here — that write is [`crate::run`]'s, and it is the only
/// thing this path leaves out.
///
/// # The two preconditions, and why a failure is `not-run` rather than a verdict
///
/// 1. **There must be a repository on disk.** A pre-opened or in-memory store has
///    no tree to read an authored layer from. (This is the same rule
///    `debt_ignore_for` follows for a project's `roteiro.toml`: substituting some
///    other repository's files would answer confidently about the wrong thing.)
///
/// 2. **The graph must match `HEAD`.** `check` compares the authored layer
///    against the derived layer, so a graph synced from an older tree yields
///    broken links for symbols that exist and misses ones that do not — drift
///    reported against a repository state that is nobody's. `Store::sync_state`
///    records the `HEAD` tree id of the last committed sync; a worktree or index
///    sync records a `…:dirty:…`/`index:…` marker instead, so neither can be
///    mistaken for a clean committed tree.
///
/// Both refuse rather than degrade, for the reason the CLI already gives for
/// declining to serve gates from a stale graph: for a gate, a
/// stale-but-unrefreshed graph is a confident wrong verdict, and a hard refusal
/// is the honest answer.
///
/// # Why the persisted graph gives the same verdict as a fresh gate run
///
/// A served store holds more than the derived layer: `build_graph` applies the
/// authored layer and then re-applies the import layers, so `adr:` nodes,
/// `authored` edges and imported nodes are all already in it, whereas the CLI
/// gate validates *before* `reapply_imports`. That difference cannot move the
/// verdict, because the imports are namespaced away from everything a link can
/// name: an `[[…]]` link resolves only to `sym:<lang>:<path>#<name>` or
/// `file:<path>` (`resolve_target`) and a `@rto:` annotation only to `adr:<id>`,
/// while imports produce `graphify:<id>` and `lat:<path>`. The already-applied
/// authored nodes are the ones `run` would have applied from the same tree — the
/// `HEAD`-match precondition above is what makes "the same tree" true — and
/// [`validate`]'s overlay reaches the same answer for them either way.
///
/// # Errors
/// Returns [`StoreError`](rto_graph::StoreError) if querying the store fails.
/// Everything that can go wrong *outside* the store — no repository, no `HEAD`,
/// an unreadable tree, a stale graph — is a `not-run` document rather than an
/// error, because those are answers the caller must be able to read.
pub fn tool_check(store: &Store, root: Option<&Path>) -> Result<ToolCheck, rto_graph::StoreError> {
    let Some(root) = root else {
        return Ok(ToolCheck::not_run(
            "this project has no repository on disk to read the authored layer from \
             (the graph was opened directly), so `check` cannot run"
                .to_owned(),
        ));
    };
    let repo = match Repo::discover(root) {
        Ok(repo) => repo,
        Err(e) => {
            return Ok(ToolCheck::not_run(format!(
                "cannot open the repository at {}: {e}",
                root.display()
            )));
        }
    };
    let head = match repo.head_tree_id() {
        Ok(tree) => tree,
        Err(e) => {
            return Ok(ToolCheck::not_run(format!(
                "cannot read the HEAD tree of {}: {e}",
                root.display()
            )));
        }
    };
    match store.sync_state()? {
        Some(synced) if synced == head => {}
        Some(synced) => {
            return Ok(ToolCheck::not_run(format!(
                "the graph was synced from `{synced}` but HEAD is `{head}`, so a drift \
                 verdict would describe neither tree — run `roteiro sync` (or restart \
                 the server) and ask again"
            )));
        }
        None => {
            return Ok(ToolCheck::not_run(
                "the graph records no synced tree, so there is nothing to check the \
                 authored layer against — run `roteiro sync`"
                    .to_owned(),
            ));
        }
    }

    let layer = match authored_layer(&repo, GraphSource::Committed) {
        Ok(layer) => layer,
        Err(e) => {
            return Ok(ToolCheck::not_run(format!(
                "cannot read the authored layer from {}: {e}",
                root.display()
            )));
        }
    };
    let mut validation = validate(store, &layer.docs, &layer.blueprints, &layer.annotations)?;
    // A malformed ADR is drift, exactly as it is for the CLI gate.
    validation.report.violations.extend(layer.malformed);
    // …and the house-style conventions, so the tool surface and the CLI gate
    // report the same drift. A model told a different number than `roteiro check`
    // prints has no way to tell which one is the repository's actual state.
    validation.report.violations.extend(layer.conventions);

    let gate = if validation.report.has_violations() {
        Gate::Fail
    } else {
        Gate::Pass
    };
    Ok(ToolCheck {
        schema: TOOL_CHECK_SCHEMA,
        gate,
        report: Some(validation.report),
        checked_against: Some(CheckedAgainst {
            source: GraphSource::Committed.as_str(),
            tree: head,
        }),
        not_run_reason: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Gate, TOOL_CHECK_SCHEMA, ToolCheck, tool_check};
    use rto_graph::{FactSet, Node, NodeKind, Repo, Store};
    use std::path::{Path, PathBuf};

    /// A git repo at `dir` with `files` committed. Returns its `HEAD` tree id.
    fn repo_with(dir: &Path, files: &[(&str, &str)]) -> String {
        std::fs::remove_dir_all(dir).ok();
        std::fs::create_dir_all(dir).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args([
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "user.email=t@example.com",
                    "-c",
                    "user.name=T",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(dir)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed in {}", dir.display());
        };
        git(&["init", "-q"]);
        for (path, body) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(&full, body).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "seed"]);
        Repo::discover(dir).unwrap().head_tree_id().unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("rto-toolcheck-{name}-{}", std::process::id()))
    }

    /// A store holding `facts`, recorded as synced from `tree` — the shape a
    /// committed `roteiro sync` leaves behind.
    fn synced(facts: &FactSet, tree: &str) -> Store {
        let mut store = Store::open_in_memory().expect("store");
        store.rebuild(facts, Some(tree)).expect("rebuild");
        store
    }

    const ADR_OK: &str = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001\n\n                          ## Design\n\nUses [[src/store.rs#Store]].\n";
    const ADR_BROKEN: &str = "---\nadr-id: \"0001\"\nstatus: Accepted\n---\n\n# ADR-0001\n\n                              ## Design\n\nUses [[src/store.rs#Ghost]].\n";

    fn derived() -> FactSet {
        FactSet::new()
            .with_node(Node::new("file:src/store.rs", NodeKind::File, "store.rs"))
            .with_node(Node::new(
                "sym:rust:src/store.rs#Store",
                NodeKind::Struct,
                "Store",
            ))
    }

    #[test]
    fn a_clean_repository_passes_and_says_what_it_checked() {
        let dir = tmp("pass");
        let tree = repo_with(
            &dir,
            &[
                ("src/store.rs", "pub struct Store;\n"),
                ("docs/adr/0001.md", ADR_OK),
            ],
        );
        let store = synced(&derived(), &tree);

        let out = tool_check(&store, Some(&dir)).expect("tool_check");
        assert_eq!(out.gate, Gate::Pass, "{out:?}");
        let report = out.report.expect("a check that ran has a report");
        assert_eq!(report.adrs, 1);
        assert_eq!(report.links_ok, 1, "{:?}", report.violations);
        assert!(report.violations.is_empty(), "{:?}", report.violations);
        let against = out.checked_against.expect("checked_against");
        assert_eq!(against.source, "committed");
        assert_eq!(against.tree, tree);
        assert!(out.not_run_reason.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drift_fails_the_gate_and_is_reported_in_full() {
        let dir = tmp("fail");
        let tree = repo_with(
            &dir,
            &[
                ("src/store.rs", "pub struct Store;\n"),
                ("docs/adr/0001.md", ADR_BROKEN),
            ],
        );
        let store = synced(&derived(), &tree);

        let out = tool_check(&store, Some(&dir)).expect("tool_check");
        assert_eq!(out.gate, Gate::Fail, "{out:?}");
        let report = out.report.expect("report");
        assert_eq!(report.violations.len(), 1, "{:?}", report.violations);
        assert_eq!(
            report.violations[0].kind,
            crate::ViolationKind::BrokenLink,
            "{:?}",
            report.violations
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The read-only path must leave the store exactly as it found it — no
    /// authored edges woven, no ADR structure applied. That write belongs to the
    /// gate ([`crate::run`]) and to nothing on a tool surface.
    #[test]
    fn checking_writes_nothing_to_the_store() {
        let dir = tmp("readonly");
        let tree = repo_with(
            &dir,
            &[
                ("src/store.rs", "pub struct Store;\n"),
                ("docs/adr/0001.md", ADR_OK),
            ],
        );
        let store = synced(&derived(), &tree);
        let before = (
            store.node_count().unwrap(),
            store.edge_count().unwrap(),
            store.all_edges().unwrap(),
        );

        let out = tool_check(&store, Some(&dir)).expect("tool_check");
        assert_eq!(out.gate, Gate::Pass);
        assert_eq!(store.node_count().unwrap(), before.0, "nodes changed");
        assert_eq!(store.edge_count().unwrap(), before.1, "edges changed");
        assert_eq!(store.all_edges().unwrap(), before.2, "edges changed");
        assert!(
            store.get_node("adr:0001").unwrap().is_none(),
            "the ADR node must not have been applied by a read-only check",
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A graph synced from another tree describes a repository state that is
    /// nobody's. Refuse, naming both trees — never a verdict.
    #[test]
    fn a_stale_graph_refuses_rather_than_reporting_drift_against_the_wrong_tree() {
        let dir = tmp("stale");
        let tree = repo_with(
            &dir,
            &[
                ("src/store.rs", "pub struct Store;\n"),
                ("docs/adr/0001.md", ADR_OK),
            ],
        );
        let store = synced(&derived(), "0000000000000000000000000000000000000000");

        let out = tool_check(&store, Some(&dir)).expect("tool_check");
        assert_eq!(out.gate, Gate::NotRun, "{out:?}");
        assert!(out.report.is_none(), "a not-run check has no report");
        let reason = out.not_run_reason.expect("reason");
        assert!(reason.contains(&tree), "names HEAD's tree: {reason}");
        assert!(
            reason.contains("0000000"),
            "names the synced tree: {reason}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_project_with_no_repository_reports_not_run_and_no_report() {
        let store = Store::open_in_memory().expect("store");
        let out = tool_check(&store, None).expect("tool_check");
        assert_eq!(out.gate, Gate::NotRun);
        assert!(out.report.is_none(), "a not-run check has no report");
        assert!(
            out.not_run_reason
                .as_deref()
                .is_some_and(|r| r.contains("no repository on disk")),
            "{:?}",
            out.not_run_reason
        );
    }

    /// A store that records no synced tree cannot be checked against one. It is
    /// reachable from a real session: `--sync-on-access` opens a project's graph
    /// lazily, and a graph that has never synced is empty, not clean.
    #[test]
    fn an_unsynced_graph_refuses_rather_than_reporting_a_clean_repository() {
        let dir = tmp("unsynced");
        repo_with(&dir, &[("src/store.rs", "pub struct Store;\n")]);
        let store = Store::open_in_memory().expect("store");

        let out = tool_check(&store, Some(&dir)).expect("tool_check");
        assert_eq!(out.gate, Gate::NotRun, "{out:?}");
        assert!(
            out.not_run_reason
                .as_deref()
                .is_some_and(|r| r.contains("no synced tree")),
            "{:?}",
            out.not_run_reason
        );
        assert!(out.report.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The defect this shape exists to prevent: a caller reading `violations`
    /// must not see an empty list when nothing was checked. The `not-run`
    /// document must not carry the key at all.
    #[test]
    fn a_not_run_document_cannot_be_read_as_zero_violations() {
        let out = ToolCheck::not_run("nope".to_owned());
        let json: serde_json::Value = serde_json::to_value(&out).expect("json");
        assert_eq!(json["schema"], TOOL_CHECK_SCHEMA);
        assert_eq!(json["gate"], "not-run");
        assert!(
            json.get("report").is_none(),
            "`report` must be absent, not an empty report: {json}"
        );
        assert!(
            json.pointer("/report/violations").is_none(),
            "`violations` must be unreachable in a not-run document: {json}"
        );
        assert!(json.get("checked_against").is_none(), "{json}");
    }
}

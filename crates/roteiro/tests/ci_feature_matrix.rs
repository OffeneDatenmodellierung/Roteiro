//! Guard: the CI feature matrix contains a cell with `execution` OFF (#667).
//!
//! Every cell of the matrix used to carry `execution`. `checks`, `coverage` and
//! `msrv` are `--all-features`; `default-features` builds the default set, which
//! contains it; and the job **named** `no-default-features` ran
//! `--no-default-features --features execution`, turning it back on. So no
//! configuration without `execution` was ever compiled, let alone tested.
//!
//! Two things that cost, both verified on `main` rather than argued:
//!
//!   * `cargo check -p roteiro --no-default-features --features mcp` did not
//!     compile — `cannot find module or crate rto_exec`, from a render path that
//!     is gated on nothing and reached into an optional crate for a timestamp.
//!     `--features mcp` is a documented way to build the MCP server and had been
//!     broken unnoticed, because nothing built it. It then happened a **second**
//!     time while the issue was open: #671 deleted the workspace-vault renderer
//!     the issue named, and its OKF replacement reintroduced the identical error
//!     at a new line. One cell would have caught both on the commit that wrote
//!     them.
//!   * `#[cfg(feature = "execution")]` was a cfg that was **true in every job**.
//!     A test carrying it ran everywhere and proved nothing about the
//!     configuration that was broken, and the `cfg(not(feature = "execution"))`
//!     arms in `rto-render`'s MCP tests ran in no job at all.
//!
//! This is read out of `ci.yml` and `crates/roteiro/Cargo.toml` rather than
//! observed from a run, for the reason [`ci_release_pr_parity`] records about its
//! own subject: the defect is a property of the *matrix*, and a matrix with a
//! hole in it is green by construction. Nothing that runs the pipeline can see
//! what the pipeline never builds.
//!
//! The feature closure is computed from the manifest instead of matched as text,
//! because `--features exec-subprocess` enables `execution` transitively and a
//! `contains("execution")` over the command line would call that cell
//! `execution`-free. That is the same defect one level up: a check that cannot
//! observe the difference it exists to detect.
//!
//! [`ci_release_pr_parity`]: ../ci_release_pr_parity/index.html

mod common;

use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::Yaml;

/// The job whose name is a promise about what it builds.
///
/// Not "some job in the file": a cell that turns features off is only worth
/// anything where somebody looks for it, and this is the job a reader — and
/// branch protection, which lists this context by name — trusts to be the place
/// features are off.
const FEATURES_OFF_JOB: &str = "no-default-features";

/// Feature selections the project's own documentation offers, each of which some
/// job must actually build.
///
/// This is the half of the guard that survives the next feature: adding a
/// combination to the README without adding it here is what fails, rather than
/// being discovered by a user whose `cargo install` does not compile.
///
/// Written as the canonical form [`Selection::canonical`] produces, so a job that
/// spells the same set differently (`-F`, `--features=…`, comma-separated) still
/// satisfies it.
const ADVERTISED: &[(&str, &str)] = &[
    (
        "default",
        "what `cargo install roteiro` gives every user (README)",
    ),
    (
        "--all-features",
        "the scope `cargo deny`/`cargo audit` need: everything the project ships",
    ),
    (
        "--no-default-features",
        "`crates/roteiro/Cargo.toml` promises this \"still yields a binary with no \
         analyzer surface at all\" — #667 is what made that a checked claim",
    ),
    (
        "--no-default-features --features execution",
        "the ADR-0014 v1.2 posture: provisions and ingests, cannot execute",
    ),
    (
        "--no-default-features --features mcp",
        "the MCP server without an analyzer surface — the combination #667 \
         reproduced the breakage with",
    ),
];

/// Cargo subcommands that actually compile the selected feature set.
///
/// `cargo metadata`, `cargo audit` and `cargo deny` read manifests and lockfiles;
/// a matrix cell they satisfy has compiled nothing, which is precisely the kind
/// of accounting this guard exists to refuse.
const COMPILING_SUBCOMMANDS: &[&str] = &["check", "build", "clippy", "test", "llvm-cov"];

/// One cargo invocation's feature selection, normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selection {
    /// The cargo subcommand, so a `test` cell can be told from a `clippy` one.
    subcommand: String,
    all_features: bool,
    default_features: bool,
    features: BTreeSet<String>,
}

impl Selection {
    /// The spelling [`ADVERTISED`] uses: flags in cargo's own order, features
    /// sorted, and the bare default set written as `default` because it is
    /// selected by passing nothing at all.
    fn canonical(&self) -> String {
        if self.all_features {
            return "--all-features".to_owned();
        }
        let mut out = String::new();
        if !self.default_features {
            out.push_str("--no-default-features");
        }
        if !self.features.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str("--features ");
            out.push_str(&self.features.iter().cloned().collect::<Vec<_>>().join(","));
        }
        if out.is_empty() {
            "default".to_owned()
        } else {
            out
        }
    }
}

/// The `[features]` table of the `roteiro` crate: feature name -> what it enables.
///
/// `None` outside a repository checkout, on the same terms as [`common::repo_file`].
fn feature_graph() -> Option<BTreeMap<String, Vec<String>>> {
    let text = common::repo_file("crates/roteiro/Cargo.toml")?;
    let manifest: toml::Table = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("crates/roteiro/Cargo.toml is not parseable TOML: {e}"));
    let features = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("crates/roteiro/Cargo.toml declares a [features] table");
    Some(
        features
            .iter()
            .map(|(name, enables)| {
                let list = enables
                    .as_array()
                    .unwrap_or_else(|| panic!("feature `{name}` is not a list"))
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect();
                (name.clone(), list)
            })
            .collect(),
    )
}

/// Every feature of the `roteiro` crate this selection turns on, transitively.
///
/// Only *this crate's* own features are followed: `dep:tokio` names a dependency
/// and `rto-render/mcp` names another crate's feature, and neither can enable
/// `roteiro`'s `execution`. Cargo also makes each optional dependency an implicit
/// feature, which cannot enable anything here either.
fn enabled_features(
    selection: &Selection,
    graph: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    if selection.all_features {
        return graph.keys().cloned().collect();
    }
    let mut queue: Vec<String> = selection.features.iter().cloned().collect();
    if selection.default_features {
        queue.push("default".to_owned());
    }
    let mut seen = BTreeSet::new();
    while let Some(name) = queue.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(enables) = graph.get(&name) {
            for next in enables {
                if !next.contains(':') && !next.contains('/') {
                    queue.push(next.clone());
                }
            }
        }
    }
    seen.remove("default");
    seen
}

/// The compiling cargo invocations in one `run:` script, with their feature
/// selection parsed out.
///
/// Handles the spellings that occur in this file and the ones that would be
/// valid if somebody reached for them: `\`-continued lines, `--features a,b`,
/// `--features=a`, `-F a`, and repeated `--features`. Everything after a bare
/// `--` is a pass-through to rustc and is not read.
fn selections_in(script: &str) -> Vec<Selection> {
    let joined = script.replace("\\\n", " ");
    let mut out = Vec::new();
    for line in joined.lines() {
        let mut tokens = line.split_whitespace();
        if tokens.next() != Some("cargo") {
            continue;
        }
        let Some(subcommand) = tokens.next() else {
            continue;
        };
        if !COMPILING_SUBCOMMANDS.contains(&subcommand) {
            continue;
        }
        let mut selection = Selection {
            subcommand: subcommand.to_owned(),
            all_features: false,
            default_features: true,
            features: BTreeSet::new(),
        };
        let mut rest = tokens.peekable();
        let add = |raw: &str, into: &mut BTreeSet<String>| {
            for name in raw.split([',', ' ']).filter(|s| !s.is_empty()) {
                into.insert(name.to_owned());
            }
        };
        while let Some(token) = rest.next() {
            match token {
                "--" => break,
                "--all-features" => selection.all_features = true,
                "--no-default-features" => selection.default_features = false,
                "--features" | "-F" => {
                    if let Some(value) = rest.next() {
                        add(value, &mut selection.features);
                    }
                }
                _ => {
                    if let Some(value) = token.strip_prefix("--features=") {
                        add(value, &mut selection.features);
                    }
                }
            }
        }
        out.push(selection);
    }
    out
}

/// The parsed `jobs:` mapping of `ci.yml`, or `None` outside a checkout.
fn ci_jobs() -> Option<Vec<(String, Yaml)>> {
    let text = common::repo_file(".github/workflows/ci.yml")?;
    let docs = yaml_rust2::YamlLoader::load_from_str(&text)
        .unwrap_or_else(|e| panic!("ci.yml is not parseable YAML: {e}"));
    let doc = docs.first().expect("ci.yml is an empty YAML document");
    let jobs = doc
        .as_hash()
        .and_then(|h| h.get(&Yaml::String("jobs".to_owned())))
        .and_then(Yaml::as_hash)
        .expect("ci.yml has no `jobs:` mapping");
    Some(
        jobs.iter()
            .filter_map(|(name, job)| Some((name.as_str()?.to_owned(), job.clone())))
            .collect(),
    )
}

/// Every compiling cargo invocation in `ci.yml`, tagged with the job it is in.
fn matrix() -> Option<Vec<(String, Selection)>> {
    let jobs = ci_jobs()?;
    let mut out = Vec::new();
    for (job, spec) in jobs {
        let steps = spec
            .as_hash()
            .and_then(|h| h.get(&Yaml::String("steps".to_owned())))
            .and_then(Yaml::as_vec)
            .map_or_else(Vec::new, Clone::clone);
        for step in steps {
            let Some(script) = step
                .as_hash()
                .and_then(|h| h.get(&Yaml::String("run".to_owned())))
                .and_then(Yaml::as_str)
            else {
                continue;
            };
            for selection in selections_in(script) {
                out.push((job.clone(), selection));
            }
        }
    }
    assert!(
        !out.is_empty(),
        "no compiling cargo invocation was found anywhere in ci.yml. This guard \
         reads the matrix out of the file, so an empty read is a green that means \
         \"could not look\" — which is exactly the shape of failure #667 was."
    );
    Some(out)
}

/// The job named `no-default-features` must build at least one configuration in
/// which `execution` is genuinely absent — and must **run** one.
///
/// Both halves matter and they are not the same assertion. Compiling the
/// configuration is what catches an unconditional `rto_exec::` reference; running
/// it is what exercises the `cfg(not(feature = "execution"))` arms, which a
/// clippy-only cell compiles and never executes. Before #667 this job satisfied
/// neither: it passed `--features execution`.
#[test]
fn the_matrix_has_a_cell_where_execution_is_absent() {
    let (Some(cells), Some(graph)) = (matrix(), feature_graph()) else {
        return; // not a source checkout
    };

    let in_job: Vec<&(String, Selection)> = cells
        .iter()
        .filter(|(job, _)| job == FEATURES_OFF_JOB)
        .collect();
    assert!(
        !in_job.is_empty(),
        "ci.yml has no job `{FEATURES_OFF_JOB}` that compiles anything. If the job \
         was renamed, rename it in branch protection too — a required context that \
         names no job leaves every PR pending with nothing showing red."
    );

    let without: Vec<&&(String, Selection)> = in_job
        .iter()
        .filter(|(_, sel)| !enabled_features(sel, &graph).contains("execution"))
        .collect();
    let built: Vec<String> = in_job.iter().map(|(_, sel)| sel.canonical()).collect();
    assert!(
        !without.is_empty(),
        "the job `{FEATURES_OFF_JOB}` builds {built:?}, and every one of them ends up \
         with `execution` enabled. That is #667 exactly: a job named for turning \
         features off that turns this one back on, in a matrix where `checks`, \
         `coverage` and `msrv` are --all-features and `default-features` carries it \
         by default. With no cell without it, `--no-default-features --features mcp` \
         stopped compiling on `main` and nothing noticed, and every \
         `#[cfg(feature = \"execution\")]` test became a test that cannot fail."
    );

    let tested: Vec<&&&(String, Selection)> = without
        .iter()
        .filter(|(_, sel)| sel.subcommand == "test")
        .collect();
    assert!(
        !tested.is_empty(),
        "`{FEATURES_OFF_JOB}` compiles {:?} without `execution` but runs no tests at \
         that feature set. `rto-render`'s `cfg(not(feature = \"execution\"))` arms \
         live in test bodies: a clippy cell compiles them and still never runs one, \
         which is the vacuity #667 is about with an extra step.",
        without
            .iter()
            .map(|(_, sel)| sel.canonical())
            .collect::<Vec<_>>()
    );
}

/// Every feature combination the documentation offers is built by some job.
///
/// The property is *every advertised feature combination compiles*. One extra
/// arbitrary combination does not establish it — so the set is enumerated, and
/// adding a combination to the README without adding it to [`ADVERTISED`] and to
/// `ci.yml` is what fails.
#[test]
fn every_advertised_feature_combination_is_built() {
    let Some(cells) = matrix() else {
        return; // not a source checkout
    };

    let built: BTreeSet<String> = cells.iter().map(|(_, sel)| sel.canonical()).collect();
    let missing: Vec<&(&str, &str)> = ADVERTISED
        .iter()
        .filter(|(combination, _)| !built.contains(*combination))
        .collect();
    assert!(
        missing.is_empty(),
        "ci.yml builds {built:?}, which does not include {}. Each of these is a \
         configuration this project tells someone to build, and a documented \
         combination nothing compiles is one a user finds broken — that is how \
         `--no-default-features --features mcp` came to fail on `main` unnoticed \
         (#667).",
        missing
            .iter()
            .map(|(combination, why)| format!("`{combination}` ({why})"))
            .collect::<Vec<_>>()
            .join("; ")
    );
}

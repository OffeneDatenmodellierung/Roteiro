//! Every public enum in the workspace either opts into being extensible
//! (`#[non_exhaustive]`) or says in its own docs why its set is closed.
//!
//! # Why this is here and not in one crate
//!
//! #391 added this rule to `rto-remote`, where it scanned **that crate's own
//! sources** — eight enums of the workspace's hundred and eight. `rto-graph`,
//! `rto-spec`, `rto-exec`, `rto-serve`, `rto-render`, `rto-llama` and `roteiro`
//! were unguarded, which is how `rto_spec::ViolationKind` — a plain-derive enum
//! whose kebab-case variant names are a **wire format a model reads** over the
//! MCP tool surface — gained a variant in #438 and forced a major (#431).
//!
//! A crate-local test cannot see that. This one walks every crate's `src/`, so a
//! public enum added anywhere has to make the decision.
//!
//! # The grandfathered list, and why it is a list rather than a threshold
//!
//! Widening the scan fails on 84 existing enums. Marking all 84
//! `#[non_exhaustive]` in one change would be 84 judgement calls made at once,
//! several of them wrong — some are error enums where exhaustive matching is
//! genuinely wanted. So they are named explicitly instead.
//!
//! Naming them is the point. A count would let one escaped enum hide behind
//! another's fix; the list cannot, because [`the_grandfathered_list_only_shrinks`]
//! fails on a **stale** entry too — one that has since been marked, justified,
//! renamed or deleted. So the list can only get shorter, and every removal is a
//! deliberate act recorded in a diff.
//!
//! `rto-remote`'s own test stays: it also covers types re-exported from outside
//! `src/`, which a directory walk cannot see.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Public enums that predate the widened scan, as `(path, enum name)`.
///
/// **Only ever remove from this list.** Adding to it means a new public enum
/// dodged the decision, which is the thing being guarded against.
const GRANDFATHERED: &[(&str, &str)] = &[
    ("crates/roteiro/src/init.rs", "HookOutcome"),
    ("crates/roteiro/src/review_llm.rs", "ReviewArm"),
    ("crates/rto-exec/src/adapter/clippy.rs", "FeatureSet"),
    ("crates/rto-exec/src/assets.rs", "AssetKind"),
    ("crates/rto-exec/src/boxlite.rs", "ImageSource"),
    ("crates/rto-exec/src/boxlite.rs", "SandboxProbe"),
    ("crates/rto-exec/src/lint_grant.rs", "Requested"),
    ("crates/rto-exec/src/runner.rs", "Consent"),
    ("crates/rto-exec/src/sandbox_store.rs", "Attribution"),
    ("crates/rto-exec/src/sandbox_store.rs", "Scope"),
    ("crates/rto-exec/src/tool_security.rs", "Coverage"),
    ("crates/rto-exec/src/tool_security.rs", "Readiness"),
    ("crates/rto-graph/src/audio.rs", "Exactness"),
    ("crates/rto-graph/src/cache.rs", "CacheError"),
    ("crates/rto-graph/src/codegraph.rs", "OracleError"),
    ("crates/rto-graph/src/compile_claim.rs", "Conclusion"),
    ("crates/rto-graph/src/compile_claim.rs", "Features"),
    ("crates/rto-graph/src/compile_claim.rs", "Suppression"),
    ("crates/rto-graph/src/compile_claim.rs", "TargetOs"),
    ("crates/rto-graph/src/compile_claim.rs", "Targets"),
    ("crates/rto-graph/src/findings.rs", "EnvironmentPolicy"),
    ("crates/rto-graph/src/findings.rs", "FindingsError"),
    ("crates/rto-graph/src/findings.rs", "Isolation"),
    ("crates/rto-graph/src/findings.rs", "RunnerKind"),
    ("crates/rto-graph/src/findings.rs", "Severity"),
    ("crates/rto-graph/src/findings.rs", "WorktreeAccess"),
    ("crates/rto-graph/src/git.rs", "ChangeStatus"),
    ("crates/rto-graph/src/git.rs", "GitError"),
    ("crates/rto-graph/src/git.rs", "GraphSource"),
    ("crates/rto-graph/src/markers.rs", "MarkerCategory"),
    ("crates/rto-graph/src/media.rs", "MediaError"),
    ("crates/rto-graph/src/media.rs", "MediaKind"),
    ("crates/rto-graph/src/media.rs", "MediaOutcome"),
    ("crates/rto-graph/src/media/gate.rs", "GateReason"),
    ("crates/rto-graph/src/memory.rs", "AnchorState"),
    ("crates/rto-graph/src/memory.rs", "Decay"),
    ("crates/rto-graph/src/memory.rs", "MemoryError"),
    ("crates/rto-graph/src/memory.rs", "MemoryKind"),
    ("crates/rto-graph/src/model.rs", "Direction"),
    ("crates/rto-graph/src/model.rs", "EdgeKind"),
    ("crates/rto-graph/src/model.rs", "NodeKind"),
    ("crates/rto-graph/src/model_choice.rs", "ModelChoiceError"),
    ("crates/rto-graph/src/model_choice.rs", "ModelSource"),
    ("crates/rto-graph/src/model_choice.rs", "ModelTask"),
    ("crates/rto-graph/src/model_choice.rs", "RemoteTier"),
    ("crates/rto-graph/src/models.rs", "DownloadError"),
    ("crates/rto-graph/src/models.rs", "DownloadEvent"),
    ("crates/rto-graph/src/models.rs", "ModelKind"),
    ("crates/rto-graph/src/models.rs", "ModelRole"),
    ("crates/rto-graph/src/models.rs", "Platform"),
    ("crates/rto-graph/src/models.rs", "RangeKind"),
    ("crates/rto-graph/src/models.rs", "RangeReply<R>"),
    ("crates/rto-graph/src/models.rs", "ResourceTier"),
    ("crates/rto-graph/src/provenance.rs", "Provenance"),
    ("crates/rto-graph/src/query.rs", "CouplingOrder"),
    ("crates/rto-graph/src/query.rs", "DensityOrder"),
    ("crates/rto-graph/src/query.rs", "RedactionState"),
    ("crates/rto-graph/src/review_corpus.rs", "CorpusError"),
    ("crates/rto-graph/src/review_corpus.rs", "DefectClass"),
    ("crates/rto-graph/src/review_corpus.rs", "Verdict"),
    ("crates/rto-graph/src/review_score.rs", "ScoreError"),
    ("crates/rto-graph/src/store.rs", "StoreError"),
    ("crates/rto-graph/src/sync.rs", "SyncError"),
    ("crates/rto-graph/src/workspace.rs", "Follow"),
    ("crates/rto-graph/src/workspace.rs", "WorkspaceError"),
    ("crates/rto-llama/src/engine.rs", "EngineError"),
    ("crates/rto-llama/src/engine.rs", "FinishReason"),
    ("crates/rto-llama/src/thinking.rs", "Unterminated"),
    ("crates/rto-render/src/lib.rs", "Target"),
    ("crates/rto-render/src/mcp.rs", "Advertised"),
    ("crates/rto-render/src/mcp.rs", "RestrictError"),
    ("crates/rto-serve/src/openai_params.rs", "Forward"),
    ("crates/rto-serve/src/openai_params.rs", "Mention"),
    ("crates/rto-serve/src/openai_params.rs", "Support"),
    ("crates/rto-serve/src/types.rs", "ContentPart"),
    ("crates/rto-serve/src/types.rs", "EmbeddingInput"),
    ("crates/rto-serve/src/types.rs", "MessageContent"),
    ("crates/rto-spec/src/adr.rs", "AdrStatus"),
    ("crates/rto-spec/src/adr.rs", "ParseError"),
    ("crates/rto-spec/src/check.rs", "ViolationKind"),
    ("crates/rto-spec/src/import.rs", "ImportError"),
    ("crates/rto-spec/src/site.rs", "ParseError"),
    ("crates/rto-spec/src/tool_check.rs", "Gate"),
];

/// One public enum found by the scan.
struct PubEnum {
    path: String,
    name: String,
    /// Carries `#[non_exhaustive]`, or documents itself as deliberately closed.
    decided: bool,
}

/// Every `.rs` file under every crate's `src/`.
///
/// Every I/O failure here is **fatal**, deliberately.
///
/// A guard that skips what it cannot read reports the same "all clear" as a guard
/// that read everything and found nothing — which is the exact failure mode this
/// file exists to prevent, turned on itself. An unreadable directory would let a
/// whole crate's public enums pass unnoticed, and the scan floor would not
/// necessarily catch it: dropping one crate of nine leaves well over 100 enums.
fn sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let rd = fs::read_dir(dir).unwrap_or_else(|e| {
            panic!(
                "cannot read {} while scanning for public enums: {e}",
                dir.display()
            )
        });
        for entry in rd {
            let entry =
                entry.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", dir.display()));
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/");
    let mut out = Vec::new();
    let mut dirs: Vec<PathBuf> = fs::read_dir(crates)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", crates.display()))
        .map(|e| {
            e.unwrap_or_else(|e| panic!("cannot read an entry of {}: {e}", crates.display()))
                .path()
                .join("src")
        })
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(
        dirs.len() >= 8,
        "found only {} crate `src/` directories; the workspace layout has changed \
         and this scan is no longer looking where the code is",
        dirs.len()
    );
    for d in &dirs {
        walk(d, &mut out);
    }
    out.sort();
    out
}

/// Scan the workspace for `pub enum` declarations and whether each has decided.
fn public_enums() -> Vec<PubEnum> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let mut found = Vec::new();
    for file in sources() {
        // Fatal, for the reason [`sources`] gives: a source file skipped because
        // it could not be read is a source file whose public enums went unchecked,
        // and the test would still report success.
        let text = fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(rest) = line.strip_prefix("pub enum ") else {
                continue;
            };
            let name = rest
                .split_whitespace()
                .next()
                .unwrap_or(rest)
                .trim_end_matches(['{', '<'])
                .to_owned();

            // Attributes and docs are collected APART, not joined. A deliberately
            // exhaustive enum's doc comment necessarily contains the text
            // `#[non_exhaustive]` — saying "not `#[non_exhaustive]`" names the
            // attribute — so a search over the joined block reports such an enum
            // as both marked and unmarked. This is the same trap #391 hit; it is
            // repeated here because the fix is not obvious from the outside.
            let (mut attrs, mut docs) = (Vec::new(), Vec::new());
            for above in lines[..i].iter().rev() {
                let t = above.trim_start();
                if t.starts_with("///") {
                    docs.push(t);
                } else if t.starts_with("#[") || t.starts_with(')') {
                    attrs.push(t);
                } else if !t.starts_with("//") || t.starts_with("//!") {
                    break;
                }
            }
            let marked = attrs.contains(&"#[non_exhaustive]");
            // The opt-out must *name* the attribute, so it reads as a refusal
            // rather than as prose that happens to sit nearby.
            let justified = docs.iter().any(|d| d.contains("not `#[non_exhaustive]`"));
            assert!(
                !(marked && justified),
                "{rel}: `pub enum {name}` both carries `#[non_exhaustive]` and documents \
                 itself as deliberately not; one of the two is stale"
            );
            found.push(PubEnum {
                path: rel.clone(),
                name,
                decided: marked || justified,
            });
        }
    }
    found
}

#[test]
fn every_public_enum_either_is_non_exhaustive_or_says_why_not() {
    let all = public_enums();
    // A floor, because a scan that stops matching would otherwise pass by finding
    // nothing — the failure mode a guard must never have.
    assert!(
        all.len() >= 100,
        "only found {} public enums across the workspace; the scan has stopped \
         matching, and a guard that finds nothing passes",
        all.len()
    );
    let allowed: BTreeSet<(&str, &str)> = GRANDFATHERED.iter().copied().collect();
    let escaped: Vec<String> = all
        .iter()
        .filter(|e| !e.decided)
        .filter(|e| !allowed.contains(&(e.path.as_str(), e.name.as_str())))
        .map(|e| format!("{}: pub enum {}", e.path, e.name))
        .collect();
    assert!(
        escaped.is_empty(),
        "these public enums are neither `#[non_exhaustive]` nor documented as \
         deliberately exhaustive, and are not grandfathered. These crates are \
         published, so a variant added later is a breaking change. Take the \
         attribute, or say in the enum's own docs why its set is closed — do not \
         add it to `GRANDFATHERED`, which exists only for enums that predate the \
         rule:\n  {}",
        escaped.join("\n  ")
    );
}

#[test]
fn the_grandfathered_list_only_shrinks() {
    let all = public_enums();
    let undecided: BTreeSet<(String, String)> = all
        .iter()
        .filter(|e| !e.decided)
        .map(|e| (e.path.clone(), e.name.clone()))
        .collect();
    let stale: Vec<String> = GRANDFATHERED
        .iter()
        .filter(|(p, n)| !undecided.contains(&((*p).to_owned(), (*n).to_owned())))
        .map(|(p, n)| format!("{p}: {n}"))
        .collect();
    assert!(
        stale.is_empty(),
        "these `GRANDFATHERED` entries no longer name an undecided public enum — \
         each has been marked, justified, renamed or deleted. Remove them: an \
         entry kept after its enum decided is an exemption held open for whatever \
         is written there next:\n  {}",
        stale.join("\n  ")
    );
}

//! `roteiro init`: scaffold the graph and install the automation around it — the
//! `post-checkout` / `post-merge` / `post-commit` freshness hooks, a `pre-commit`
//! drift gate (Stage 16), an `AGENTS.md` snippet, and an installable agent
//! `SKILL.md`.
//!
//! Everything written here is *managed*: each artifact carries a marker so a
//! re-run updates Roteiro's own content in place and never clobbers a user's
//! existing (foreign) hook or notes.
//!
//! `AGENTS.md` is the lean, always-on context; the `SKILL.md` is the deeper,
//! on-demand operational guide (loaded only when relevant). The skill goes to the
//! cross-tool `.agents/skills/roteiro/` location, plus GitHub's
//! `.github/skills/roteiro/` when the repo already uses `.github` (its Copilot
//! reviewer reads that path).

use std::fs;
use std::io;
use std::path::Path;

/// Marker identifying a Roteiro-managed git hook.
const HOOK_MARKER: &str = "roteiro-managed";
/// Marker identifying the Roteiro block in `AGENTS.md`.
const AGENTS_MARKER: &str = "<!-- roteiro-managed -->";

/// The git hooks Roteiro installs. `post-checkout`/`post-merge`/`post-commit`
/// keep the graph fresh after `HEAD` moves; `pre-commit` gates a commit that
/// introduces authored-vs-code drift (Stage 16).
pub const MANAGED_HOOKS: &[&str] = &["post-checkout", "post-merge", "post-commit", "pre-commit"];

/// The content of a managed hook `name`. Every hook guards on `roteiro` being
/// installed, so it is safe on machines without the tool.
///
/// `pre-commit` runs the **worktree-aware** `check` and blocks the commit on
/// drift (non-zero exit); `git commit --no-verify` skips it. The freshness hooks
/// run `sync --committed` and never fail the git operation.
///
/// When `fetch` is set (`roteiro init --fetch`), the freshness hooks first try to
/// download the CI-published graph artifact and `load` it — a fast path that
/// skips local extraction — falling back to a rebuild if `gh` is absent, the
/// download fails, or the artifact's tree does not match `HEAD` (`load` refuses a
/// stale artifact). Network only happens with this opt-in.
///
/// When `vault` is set (`roteiro init --vault`), the freshness hooks additionally
/// regenerate the local Obsidian vault from the fresh graph. `pre-commit` is
/// unaffected by either flag.
#[must_use]
pub fn hook_script(name: &str, fetch: bool, vault: bool) -> String {
    let header = format!(
        "#!/bin/sh\n\
         # {HOOK_MARKER}: Roteiro knowledge-graph automation.\n\
         # Delete this file to disable. Re-run `roteiro init` to reinstall.\n"
    );
    if name == "pre-commit" {
        return format!(
            "{header}\
             # Block a commit that introduces ADR/annotation drift. Validates the\n\
             # staged index — exactly what this commit will record. Skip once with\n\
             # `git commit --no-verify`.\n\
             command -v roteiro >/dev/null 2>&1 || exit 0\n\
             roteiro check --staged || {{\n\
             \techo 'roteiro: commit blocked by knowledge-graph drift (see above); \
             use `git commit --no-verify` to override.' >&2\n\
             \texit 1\n\
             }}\n"
        );
    }
    // Freshness hook: refresh the graph after HEAD moves, then — when
    // `roteiro init --vault` opted in — regenerate the local Obsidian vault so it
    // tracks the graph (a gitignored build-output, never committed).
    let mut body = if fetch {
        FETCH_REFRESH.to_owned()
    } else {
        "# Keep the Roteiro knowledge graph fresh after HEAD changes.\n\
         command -v roteiro >/dev/null 2>&1 && roteiro sync --committed >/dev/null 2>&1 || true\n"
            .to_owned()
    };
    if vault {
        body.push_str(VAULT_REFRESH);
    }
    format!("{header}{body}")
}

/// Appended to the freshness hooks by `--vault`: rebuild the local Obsidian vault
/// from the now-fresh graph. `render obsidian` syncs the graph itself and writes
/// to the gitignored `vault/` dir; best-effort, never failing the git operation.
const VAULT_REFRESH: &str = concat!(
    "# Regenerate the local Obsidian vault (gitignored build-output) to match.\n",
    "command -v roteiro >/dev/null 2>&1 && roteiro render obsidian >/dev/null 2>&1 || true\n",
);

/// The freshness-hook body used with `--fetch`: try the CI artifact, else rebuild.
/// Kept as a literal (no interpolation) so the shell `$tmp`/braces stay verbatim.
const FETCH_REFRESH: &str = concat!(
    "# Keep the Roteiro knowledge graph fresh after HEAD changes.\n",
    "command -v roteiro >/dev/null 2>&1 || exit 0\n",
    "# Opt-in fast path (`roteiro init --fetch`): try the CI-published graph\n",
    "# artifact before rebuilding. `roteiro load` refuses an artifact whose tree\n",
    "# does not match HEAD, so a stale asset falls through to a local rebuild.\n",
    "# A flag records success rather than `exit`ing, so any step appended below\n",
    "# (e.g. --vault's render) still runs.\n",
    "loaded=0\n",
    "if command -v gh >/dev/null 2>&1; then\n",
    "\t# Portable temp file: GNU `mktemp` needs no args; BSD/macOS needs a\n",
    "\t# template, so fall back to `-t`.\n",
    "\ttmp=$(mktemp 2>/dev/null || mktemp -t roteiro-graph 2>/dev/null) || tmp=\"\"\n",
    "\tif [ -n \"$tmp\" ] && \\\n",
    "\t\tgh release download graph-latest --pattern roteiro-graph.json \\\n",
    "\t\t\t--output \"$tmp\" --clobber >/dev/null 2>&1 && \\\n",
    "\t\troteiro load \"$tmp\" >/dev/null 2>&1; then\n",
    "\t\tloaded=1\n",
    "\tfi\n",
    "\t[ -n \"$tmp\" ] && rm -f \"$tmp\"\n",
    "fi\n",
    "# Rebuild locally only if the fast path didn't load a matching artifact.\n",
    "[ \"$loaded\" = 1 ] || roteiro sync --committed >/dev/null 2>&1 || true\n",
);

/// Whether `content` is a Roteiro-managed hook (safe to overwrite).
#[must_use]
pub fn is_managed_hook(content: &str) -> bool {
    content.contains(HOOK_MARKER)
}

/// The outcome of installing one hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    /// The hook was newly created.
    Installed,
    /// An existing Roteiro-managed hook was refreshed.
    Updated,
    /// A pre-existing foreign hook was left untouched.
    SkippedForeign,
}

/// Install (or refresh) one managed hook in `hooks_dir`, without clobbering a
/// foreign hook of the same name. `fetch` selects the artifact-fast-path freshness
/// hooks, and `vault` appends local Obsidian-vault regeneration (see
/// [`hook_script`]).
///
/// # Errors
/// Returns [`io::Error`] on filesystem failure.
pub fn install_hook(
    hooks_dir: &Path,
    name: &str,
    fetch: bool,
    vault: bool,
) -> io::Result<HookOutcome> {
    fs::create_dir_all(hooks_dir)?;
    let path = hooks_dir.join(name);
    let outcome = match fs::read_to_string(&path) {
        Ok(existing) if is_managed_hook(&existing) => HookOutcome::Updated,
        Ok(_) => return Ok(HookOutcome::SkippedForeign),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HookOutcome::Installed,
        Err(e) => return Err(e),
    };
    fs::write(&path, hook_script(name, fetch, vault))?;
    set_executable(&path)?;
    Ok(outcome)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// The Roteiro section for `AGENTS.md`, delimited by the managed marker so it
/// can be replaced in place on re-run.
#[must_use]
pub fn agents_section() -> String {
    format!(
        "{AGENTS_MARKER}\n\
         ## Roteiro knowledge graph\n\
         \n\
         This repository has a Roteiro knowledge graph — code structure, authored\n\
         intent (ADRs, blueprints) and their links in one provenance-tagged store\n\
         (every fact labelled `derived` | `authored` | `inferred`). Prefer querying\n\
         it over grepping when orienting.\n\
         \n\
         **Find, then explain:**\n\
         \n\
         - `roteiro search \"<text>\"` — ranked text search (names, keys, paths and\n\
         captured prose); curated ADRs/blueprints rank first. The offline entry point\n\
         for \"what/why\" questions — then `query` a returned key. (`roteiro serve\n\
         --models` exposes the same as a `search` tool + an OpenAI `/v1` endpoint;\n\
         MCP agents get `search`/`explain`/`path`/`debt` directly.)\n\
         - `roteiro query <key> [--json]` — a node and its provenance-labelled edges.\n\
         Keys: `sym:<lang>:<path>#<Name>`, `file:<path>`, `adr:<id>`.\n\
         - `roteiro query --kind <kind>` — list nodes of a kind (`fn`, `adr`, …).\n\
         - `roteiro context <key>` — a node's callers, callees and governing ADRs.\n\
         - `roteiro path <a> <b>` · `roteiro debt` — connections and intent-debt.\n\
         \n\
         **Plan a change:** `roteiro spec context <topic>` → `spec scaffold … --kind adr`\n\
         → `spec draft <file>` (the first two need no model), then `roteiro check`.\n\
         \n\
         **Before finishing a change:** run `roteiro review [--json]` (a graph-grounded\n\
         review of your change — callers/callees, governing ADRs, drift and blast\n\
         radius) and `roteiro check` (fails on ADR/annotation drift; a managed\n\
         `pre-commit` hook enforces it too, `git commit --no-verify` skips). `roteiro\n\
         sync` refreshes the graph (git hooks do this automatically).\n\
         \n\
         For the full operational guide, see the installed skill at\n\
         `.agents/skills/roteiro/SKILL.md`.\n\
         {AGENTS_MARKER}\n"
    )
}

/// Ensure `AGENTS.md` at `path` contains the managed Roteiro section, replacing
/// an existing managed block or appending a new one. Returns `true` if the file
/// was created or changed.
///
/// # Errors
/// Returns [`io::Error`] on filesystem failure.
pub fn ensure_agents(path: &Path) -> io::Result<bool> {
    let section = agents_section();
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };

    let updated = match managed_block_range(&existing) {
        Some((start, end)) => {
            let mut s = String::with_capacity(existing.len());
            s.push_str(&existing[..start]);
            s.push_str(section.trim_end());
            s.push_str(&existing[end..]);
            s
        }
        None if existing.is_empty() => section,
        None => {
            let mut s = existing.clone();
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s.push('\n');
            s.push_str(&section);
            s
        }
    };

    if updated == existing {
        return Ok(false);
    }
    fs::write(path, updated)?;
    Ok(true)
}

/// Byte range of the managed block (marker … marker, inclusive), if present.
fn managed_block_range(content: &str) -> Option<(usize, usize)> {
    let start = content.find(AGENTS_MARKER)?;
    let after = start + AGENTS_MARKER.len();
    let second = content[after..].find(AGENTS_MARKER)? + after;
    Some((start, second + AGENTS_MARKER.len()))
}

/// The relative sub-path, under a skills *base* dir (e.g. `.agents` or
/// `.github`), of the managed skill file: `skills/roteiro/SKILL.md`.
const SKILL_SUBPATH: [&str; 3] = ["skills", "roteiro", "SKILL.md"];

/// The canonical Roteiro agent skill, embedded at build time. Written verbatim to
/// each target skill dir. It carries the managed marker on its first line, so a
/// re-run refreshes it in place and a foreign `SKILL.md` is left untouched.
#[must_use]
pub fn skill_markdown() -> &'static str {
    include_str!("../assets/skill/SKILL.md")
}

/// Whether `content` is the Roteiro-managed skill (safe to overwrite).
#[must_use]
pub fn is_managed_skill(content: &str) -> bool {
    content.contains(HOOK_MARKER)
}

/// The path a skill installed under `base_dir` lives at
/// (`<base_dir>/skills/roteiro/SKILL.md`).
#[must_use]
pub fn skill_path(base_dir: &Path) -> std::path::PathBuf {
    SKILL_SUBPATH
        .iter()
        .fold(base_dir.to_path_buf(), |p, seg| p.join(seg))
}

/// Install (or refresh) the managed agent skill under `base_dir` (typically
/// `.agents` or `.github`), writing `<base_dir>/skills/roteiro/SKILL.md`. Mirrors
/// [`install_hook`]: a Roteiro-managed file is refreshed, a foreign one is left
/// untouched.
///
/// # Errors
/// Returns [`io::Error`] on filesystem failure.
pub fn install_skill(base_dir: &Path) -> io::Result<HookOutcome> {
    let path = skill_path(base_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let outcome = match fs::read_to_string(&path) {
        Ok(existing) if is_managed_skill(&existing) => HookOutcome::Updated,
        Ok(_) => return Ok(HookOutcome::SkippedForeign),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HookOutcome::Installed,
        Err(e) => return Err(e),
    };
    fs::write(&path, skill_markdown())?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{
        HookOutcome, agents_section, ensure_agents, hook_script, install_hook, install_skill,
        is_managed_hook, is_managed_skill, skill_markdown, skill_path,
    };

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-init-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn hook_is_recognisable_and_self_guarding() {
        let s = hook_script("post-checkout", false, false);
        assert!(is_managed_hook(&s));
        assert!(s.starts_with("#!/bin/sh"));
        assert!(s.contains("command -v roteiro"));
        assert!(
            s.contains("roteiro sync --committed"),
            "freshness hook syncs"
        );
        // The default freshness hook does not touch the network or render a vault.
        assert!(!s.contains("gh release download"));
        assert!(!s.contains("render obsidian"), "vault render is opt-in");
        assert!(!is_managed_hook("#!/bin/sh\necho other\n"));
    }

    #[test]
    fn vault_flag_appends_vault_render_to_freshness_hooks_only() {
        // `--vault` adds a vault regeneration step to the freshness hooks…
        let fresh = hook_script("post-merge", false, true);
        assert!(
            fresh.contains("roteiro render obsidian"),
            "freshness hook regenerates the vault with --vault"
        );
        assert!(
            fresh.contains("roteiro sync --committed"),
            "still syncs first"
        );
        // …and composes with --fetch. The fetch fast path must set a flag on a
        // successful load rather than `exit`ing, or the appended vault render would
        // be unreachable (regression guard, Copilot on #173).
        let fetch_vault = hook_script("post-checkout", true, true);
        assert!(
            fetch_vault.contains("roteiro render obsidian"),
            "vault render is present with --fetch --vault"
        );
        assert!(
            fetch_vault.contains("loaded=1"),
            "fetch success records a flag, not an early exit"
        );
        assert!(
            !fetch_vault.contains("; exit 0"),
            "no early `exit 0` that would short-circuit the appended vault render"
        );
        // …but never touches the pre-commit gate.
        assert!(!hook_script("pre-commit", false, true).contains("render obsidian"));
    }

    #[test]
    fn fetch_hook_tries_artifact_then_falls_back_to_sync() {
        let s = hook_script("post-merge", true, false);
        assert!(is_managed_hook(&s));
        assert!(s.contains("command -v gh"), "guards on gh being installed");
        assert!(
            s.contains("gh release download graph-latest"),
            "fetches the CI artifact"
        );
        assert!(s.contains("roteiro load"), "loads the fetched artifact");
        assert!(
            s.contains("roteiro sync --committed"),
            "falls back to a local rebuild"
        );
        // Neither `--fetch` nor `--vault` alters the pre-commit gate.
        assert_eq!(
            hook_script("pre-commit", true, true),
            hook_script("pre-commit", false, false)
        );
    }

    #[test]
    fn pre_commit_hook_gates_on_check_and_is_skippable() {
        let s = hook_script("pre-commit", false, false);
        assert!(is_managed_hook(&s));
        assert!(s.contains("command -v roteiro"), "guards on install");
        assert!(s.contains("roteiro check"), "runs the worktree-aware check");
        assert!(s.contains("exit 1"), "blocks the commit on drift");
        assert!(s.contains("--no-verify"), "documents the escape hatch");
        // Distinct from the freshness hooks — it must not just sync.
        assert!(!s.contains("roteiro sync"));
    }

    #[test]
    fn install_creates_updates_and_skips_foreign() {
        let dir = tmp("hooks");
        let hooks = dir.join("hooks");

        // First install → created.
        assert_eq!(
            install_hook(&hooks, "post-checkout", false, false).expect("install"),
            HookOutcome::Installed
        );
        let path = hooks.join("post-checkout");
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "hook should be executable");
        }

        // Second install → updated (managed).
        assert_eq!(
            install_hook(&hooks, "post-checkout", false, false).expect("reinstall"),
            HookOutcome::Updated
        );

        // A foreign hook is left untouched.
        let foreign = hooks.join("post-merge");
        std::fs::write(&foreign, "#!/bin/sh\necho mine\n").unwrap();
        assert_eq!(
            install_hook(&hooks, "post-merge", false, false).expect("skip"),
            HookOutcome::SkippedForeign
        );
        assert_eq!(
            std::fs::read_to_string(&foreign).unwrap(),
            "#!/bin/sh\necho mine\n"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn agents_created_updated_and_idempotent() {
        let dir = tmp("agents");
        let path = dir.join("AGENTS.md");

        // Created.
        assert!(ensure_agents(&path).expect("create"));
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("Roteiro knowledge graph"));

        // Idempotent: no change on re-run.
        assert!(!ensure_agents(&path).expect("noop"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

        // Appends after pre-existing content, and replaces only the managed block.
        std::fs::write(&path, "# My agents\n\nHello.\n").unwrap();
        assert!(ensure_agents(&path).expect("append"));
        let merged = std::fs::read_to_string(&path).unwrap();
        assert!(merged.starts_with("# My agents"));
        assert!(merged.contains("Roteiro knowledge graph"));
        // Exactly one managed block (two markers).
        assert_eq!(merged.matches("<!-- roteiro-managed -->").count(), 2);

        // Re-running after manual edits still yields a single managed block.
        assert!(!ensure_agents(&path).expect("noop2"));
        assert_eq!(
            agents_section().matches("<!-- roteiro-managed -->").count(),
            2
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn agents_section_points_at_search_and_the_skill() {
        let s = agents_section();
        // The lean block names the discovery entry point and the deeper guide.
        assert!(
            s.contains("`search`"),
            "search is the find-then-explain entry"
        );
        assert!(s.contains("roteiro review"), "review is still called out");
        assert!(
            s.contains(".agents/skills/roteiro/SKILL.md"),
            "points at the installed skill for depth"
        );
    }

    #[test]
    fn skill_is_managed_and_a_valid_skill_document() {
        let md = skill_markdown();
        assert!(is_managed_skill(md), "skill carries the managed marker");
        assert!(!is_managed_skill("---\nname: other\n---\n"));
        // Portable SKILL.md contract: YAML frontmatter with name + description.
        assert!(md.contains("name: roteiro"), "has a skill name");
        assert!(
            md.contains("description:"),
            "has a description for relevance"
        );
        // Teaches the graph surface it is meant to.
        assert!(md.contains("search"), "covers the search entry point");
        assert!(md.contains("provenance"), "covers the provenance model");
        assert!(md.contains("roteiro spec"), "covers the plan workflow");
        // The "Proving a negative" rule (#290): a sub-agent grepped
        // `evict|ttl|prune|capacity|max_`, found nothing, and reported that no
        // eviction idiom existed anywhere — which `roteiro search evict` refuted
        // in seconds. It was hand-added to the generated artifacts and therefore
        // absent from this template, so the next `init` deleted it from both
        // copies. It lives here now; keep it here.
        assert!(
            md.contains("## Proving a negative"),
            "the template must teach that `grep` cannot establish absence"
        );
        assert!(
            md.contains("Never assert absence from `grep` alone"),
            "the companion rule-of-thumb bullet points at that section"
        );
    }

    /// The repository root, from this crate's manifest directory
    /// (`crates/roteiro`). Mirrors `tests/ci_coverage_claims.rs`.
    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repo root is two levels above crates/roteiro")
            .to_path_buf()
    }

    /// Guard: the committed skill artifacts must be exactly what the template
    /// generates.
    ///
    /// `SKILL.md` is a *managed* file — `install_skill` overwrites it verbatim
    /// from [`skill_markdown`] on every `roteiro init`. So an edit made to the
    /// committed copy instead of to `assets/skill/SKILL.md` is not merged and
    /// not warned about; it is destroyed at the next `init`, silently and with
    /// no conflict. That is exactly how the "Proving a negative" section was
    /// lost from both copies at once.
    ///
    /// Nothing in the build could notice, because "this artifact is stale" is
    /// not a compile error. This test makes it noticeable, and fails at the
    /// place that can act on it: whoever edits the artifact gets told to edit
    /// the template and re-run `init`.
    ///
    /// It compares whole bytes rather than needles on purpose. A needle list
    /// only guards the sections someone thought to enumerate, which is the same
    /// failure again one level up — the deleted section was not on anyone's
    /// list.
    #[test]
    fn committed_skill_artifacts_match_the_template() {
        const COPIES: &[&str] = &[
            ".agents/skills/roteiro/SKILL.md",
            ".github/skills/roteiro/SKILL.md",
        ];
        let root = repo_root();
        for rel in COPIES {
            // Absent in a packaged crate — this guard is about *this* repository.
            let Ok(committed) = std::fs::read_to_string(root.join(rel)) else {
                continue;
            };
            let template = skill_markdown();
            if committed == template {
                continue;
            }
            // Name the first differing line rather than dumping two 5 KB blobs
            // into the CI log, which buries the one line that matters.
            let (got, want): (Vec<_>, Vec<_>) =
                (committed.lines().collect(), template.lines().collect());
            let n = got
                .iter()
                .zip(&want)
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| got.len().min(want.len()));
            panic!(
                "{rel} has diverged from crates/roteiro/assets/skill/SKILL.md at line {}:\n  \
                 committed: {:?}\n  template:  {:?}\n\
                 The template is written verbatim over this file by `roteiro init`, so an \
                 edit made here is deleted on the next run — silently, with no conflict. \
                 Move the change into crates/roteiro/assets/skill/SKILL.md and re-run \
                 `roteiro init` to regenerate both copies.",
                n + 1,
                got.get(n),
                want.get(n),
            );
        }
    }

    #[test]
    fn install_skill_creates_updates_and_skips_foreign() {
        let dir = tmp("skill");
        let base = dir.join(".agents");
        let path = skill_path(&base);

        // First install → created, at <base>/skills/roteiro/SKILL.md.
        assert_eq!(
            install_skill(&base).expect("install"),
            HookOutcome::Installed
        );
        assert!(path.exists());
        assert!(path.ends_with("skills/roteiro/SKILL.md"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), skill_markdown());

        // Second install → refreshed in place (managed).
        assert_eq!(
            install_skill(&base).expect("reinstall"),
            HookOutcome::Updated
        );

        // A foreign SKILL.md is left untouched.
        std::fs::write(&path, "# my own skill\n").unwrap();
        assert_eq!(
            install_skill(&base).expect("skip"),
            HookOutcome::SkippedForeign
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# my own skill\n");

        std::fs::remove_dir_all(&dir).ok();
    }
}

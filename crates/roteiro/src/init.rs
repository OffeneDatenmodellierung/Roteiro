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
/// When `okf` is set (`roteiro init --okf`), the freshness hooks additionally
/// regenerate the local OKF bundle from the fresh graph. `pre-commit` is
/// unaffected by either flag.
#[must_use]
pub fn hook_script(name: &str, fetch: bool, okf: bool) -> String {
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
    // `roteiro init --okf` opted in — regenerate the local OKF bundle so it tracks
    // the graph (a build-output, never committed).
    let mut body = if fetch {
        FETCH_REFRESH.to_owned()
    } else {
        "# Keep the Roteiro knowledge graph fresh after HEAD changes.\n\
         command -v roteiro >/dev/null 2>&1 && roteiro sync --committed >/dev/null 2>&1 || true\n"
            .to_owned()
    };
    if okf {
        body.push_str(&okf_refresh());
    }
    format!("{header}{body}")
}

/// Appended to the freshness hooks by `--okf`: rebuild the local OKF bundle from
/// the now-fresh graph. `render okf` syncs the graph itself and writes to
/// [`crate::BUNDLE_DIR`], a build-output that belongs in `.gitignore`.
///
/// A function rather than a literal so the directory is named **once**, by the
/// constant the renderer's own default reads. The message a user meets at the
/// moment the refresh failed is not the place for a second spelling of where it
/// would have written.
///
/// # Why this step reports its failure, when the ones around it do not
///
/// It used to end `>/dev/null 2>&1 || true`, and after 4.0.0 removed
/// `render obsidian` that shape hid a command which *could not succeed*: the
/// bundle silently stopped being refreshed and nothing anywhere said so. A
/// best-effort step may swallow a transient failure; it must not be able to
/// swallow a permanent one, because then nothing distinguishes "nothing to do"
/// from "this has never worked".
///
/// So stdout stays quiet — a summary line on every checkout is noise — but
/// stderr is left alone and a failing render says which command to run to see
/// why. The git operation is still never failed: the `||` branch ends the hook
/// on a success status, and git ignores a `post-*` hook's exit code regardless.
/// `exit` is deliberately not used, so a step appended after this one would
/// still run.
fn okf_refresh() -> String {
    // Any line continuation here must fall between whole words: broken mid-token
    // it would render as `okf/ bundle`, which reads like a path that does not
    // exist rather than like a sentence (Copilot, this PR).
    format!(
        "# Regenerate the local OKF bundle (a build-output; gitignore it) to match.\n\
         if command -v roteiro >/dev/null 2>&1; then\n\
         \troteiro render okf >/dev/null || echo 'roteiro: could not refresh the \
         OKF bundle in {dir}/ — run `roteiro render okf` to see why' >&2\n\
         fi\n",
        dir = crate::BUNDLE_DIR
    )
}

/// The freshness-hook body used with `--fetch`: try the CI artifact, else rebuild.
/// Kept as a literal (no interpolation) so the shell `$tmp`/braces stay verbatim.
const FETCH_REFRESH: &str = concat!(
    "# Keep the Roteiro knowledge graph fresh after HEAD changes.\n",
    "command -v roteiro >/dev/null 2>&1 || exit 0\n",
    "# Opt-in fast path (`roteiro init --fetch`): try the CI-published graph\n",
    "# artifact before rebuilding. `roteiro load` refuses an artifact whose tree\n",
    "# does not match HEAD, so a stale asset falls through to a local rebuild.\n",
    "# A flag records success rather than `exit`ing, so any step appended below\n",
    "# (e.g. --okf's render) still runs.\n",
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
/// hooks, and `okf` appends local OKF-bundle regeneration (see [`hook_script`]).
///
/// # Errors
/// Returns [`io::Error`] on filesystem failure.
pub fn install_hook(
    hooks_dir: &Path,
    name: &str,
    fetch: bool,
    okf: bool,
) -> io::Result<HookOutcome> {
    fs::create_dir_all(hooks_dir)?;
    let path = hooks_dir.join(name);
    let outcome = match fs::read_to_string(&path) {
        Ok(existing) if is_managed_hook(&existing) => HookOutcome::Updated,
        Ok(_) => return Ok(HookOutcome::SkippedForeign),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HookOutcome::Installed,
        Err(e) => return Err(e),
    };
    fs::write(&path, hook_script(name, fetch, okf))?;
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
/// each target skill dir. It carries the managed marker just below its YAML
/// frontmatter, so a re-run refreshes it in place and a foreign `SKILL.md` is
/// left untouched.
///
/// The marker sits *below* the frontmatter rather than above it because a
/// `SKILL.md` is only frontmatter if the block is at byte 0 — see
/// `skill_is_managed_and_a_valid_skill_document`. [`is_managed_skill`] searches
/// the whole document, so nothing depends on the marker being first.
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
        // The default freshness hook does not touch the network or render a
        // bundle.
        assert!(!s.contains("gh release download"));
        assert!(!s.contains("render okf"), "bundle render is opt-in");
        assert!(!is_managed_hook("#!/bin/sh\necho other\n"));
    }

    #[test]
    fn okf_flag_appends_bundle_render_to_freshness_hooks_only() {
        // `--okf` adds a bundle regeneration step to the freshness hooks…
        let fresh = hook_script("post-merge", false, true);
        // The *invocation*, not the words. Measured: a bare
        // `contains("roteiro render okf")` passes even when the hook invokes
        // `render obsidian`, because the failure message quotes the command it
        // was telling the user to run. A string that co-occurs with the property
        // is not the property.
        assert!(
            fresh.contains("\troteiro render okf >"),
            "freshness hook regenerates the bundle with --okf: {fresh}"
        );
        assert!(
            fresh.contains("roteiro sync --committed"),
            "still syncs first"
        );
        // A failing render must be audible. `|| true` here would restore exactly
        // the shape that hid `render obsidian` for a whole major version: a step
        // that cannot succeed, saying nothing.
        assert!(
            !fresh.contains("render okf >/dev/null 2>&1"),
            "the render's stderr must reach the user: {fresh}"
        );
        assert!(
            fresh.contains("could not refresh the OKF bundle in okf/ —"),
            "a failing render must name itself: {fresh}"
        );
        // …and composes with --fetch. The fetch fast path must set a flag on a
        // successful load rather than `exit`ing, or the appended bundle render
        // would be unreachable (regression guard, Copilot on #173).
        let fetch_okf = hook_script("post-checkout", true, true);
        assert!(
            fetch_okf.contains("\troteiro render okf >"),
            "bundle render is present with --fetch --okf: {fetch_okf}"
        );
        assert!(
            fetch_okf.contains("loaded=1"),
            "fetch success records a flag, not an early exit"
        );
        assert!(
            !fetch_okf.contains("; exit 0"),
            "no early `exit 0` that would short-circuit the appended bundle render"
        );
        // …but never touches the pre-commit gate.
        assert!(!hook_script("pre-commit", false, true).contains("render okf"));
    }

    /// Every `roteiro` invocation a hook shell script contains is a command this
    /// binary actually accepts.
    ///
    /// **This is the guard the file did not have.** 4.0.0 removed
    /// `render obsidian`, and the hook installed by `--okf` went on invoking it
    /// for a whole PR: `|| true` and `2>&1` meant a user saw nothing, and three
    /// unit tests *asserted the broken string*, so the suite stayed green while
    /// the product did not work. Pinning a literal proves the generator emits what
    /// it was told to; it cannot notice that what it was told is no longer a
    /// command.
    ///
    /// So this asserts against the CLI itself rather than against a list kept
    /// here. `try_parse_from` catches a removed or renamed subcommand and a flag
    /// that no longer exists; a `render` target is checked separately because the
    /// target is a positional `String` — clap accepts `render obsidian` happily,
    /// and only [`rto_render::Target::parse`] knows it is gone. That is exactly
    /// the seam the defect went through.
    #[test]
    fn every_roteiro_command_a_hook_runs_is_one_the_cli_accepts() {
        use clap::Parser as _;

        let mut checked = 0usize;
        for name in super::MANAGED_HOOKS {
            for fetch in [false, true] {
                for okf in [false, true] {
                    let script = hook_script(name, fetch, okf);
                    for argv in roteiro_invocations(&script) {
                        let shown = argv.join(" ");
                        let parsed = crate::Cli::try_parse_from(
                            std::iter::once("roteiro".to_owned()).chain(argv.iter().cloned()),
                        );
                        assert!(
                            parsed.is_ok(),
                            "hook `{name}` (fetch={fetch}, okf={okf}) runs `roteiro {shown}`, \
                             which this CLI does not accept: {:?}",
                            parsed.err().map(|e| e.to_string())
                        );
                        // clap is satisfied by any word here — the removed target
                        // was a positional string, which is how it survived.
                        if argv.first().map(String::as_str) == Some("render") {
                            let target = argv.get(1).map(String::as_str).unwrap_or_default();
                            assert!(
                                rto_render::Target::parse(target).is_some(),
                                "hook `{name}` renders target `{target}`, which is not a \
                                 render target this build has"
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
        // The extraction is the load-bearing part: a matcher that found nothing
        // would make every assertion above vacuously true.
        assert!(
            checked >= 8,
            "the scripts must contain roteiro invocations to check; found {checked}"
        );
    }

    /// Every `roteiro …` command line in a hook script, as argv without the
    /// program name.
    ///
    /// Deliberately small: hook bodies are written here, so this only has to read
    /// the shell *this file* generates. It splits on the operators those bodies
    /// use, drops redirections, and ignores `roteiro` appearing as an argument
    /// (`command -v roteiro`) rather than as the command.
    fn roteiro_invocations(script: &str) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        for line in script.lines() {
            let line = line.trim_start_matches(['\t', ' ']);
            if line.starts_with('#') {
                continue;
            }
            for part in line
                .split("&&")
                .flat_map(|p| p.split("||"))
                .flat_map(|p| p.split(';'))
                .flat_map(|p| p.split('|'))
            {
                let part = part.trim();
                // `if`/`then` prefix a command on the same line.
                let part = part
                    .strip_prefix("if ")
                    .or_else(|| part.strip_prefix("then "))
                    .unwrap_or(part)
                    .trim();
                let Some(rest) = part.strip_prefix("roteiro ") else {
                    continue;
                };
                let argv: Vec<String> = rest
                    .split_whitespace()
                    .take_while(|t| !t.starts_with('>') && !t.starts_with("2>"))
                    .map(|t| t.trim_matches('"').to_owned())
                    .collect();
                if !argv.is_empty() {
                    out.push(argv);
                }
            }
        }
        out
    }

    /// Every generated hook is valid POSIX shell.
    ///
    /// The bodies compose — `--fetch` picks one, `--okf` appends another — so a
    /// construct that parses alone can still be broken by what precedes it. This
    /// file gained its first multi-line `if … fi` in a freshness body when the
    /// bundle refresh stopped hiding its own failure, and `sh` is the only thing
    /// here that actually knows whether the result parses. A hook git cannot run
    /// fails the same way the removed command did: quietly, at a moment nobody is
    /// watching.
    #[test]
    fn every_generated_hook_is_valid_posix_shell() {
        let dir = tmp("shell-syntax");
        for name in super::MANAGED_HOOKS {
            for fetch in [false, true] {
                for okf in [false, true] {
                    let script = hook_script(name, fetch, okf);
                    let path = dir.join(format!("{name}-{fetch}-{okf}.sh"));
                    std::fs::write(&path, &script).expect("write");
                    let out = std::process::Command::new("sh")
                        .arg("-n")
                        .arg(&path)
                        .output()
                        .expect("run sh -n");
                    assert!(
                        out.status.success(),
                        "hook `{name}` (fetch={fetch}, okf={okf}) is not valid shell: {}\n{script}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
        }
    }

    /// The directory `render okf` writes by default is the directory this
    /// repository ignores.
    ///
    /// Two spellings of one fact, in files that no compiler relates: renaming the
    /// output directory while leaving `.gitignore` naming the old one is how this
    /// PR left a 8,700-file build-output untracked-and-unignored, visible only as
    /// a flooded `git status`.
    #[test]
    fn the_default_bundle_directory_is_ignored_here() {
        let ignore = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(".gitignore"),
        )
        .expect("the repository has a .gitignore");
        let entry = format!("/{}/", crate::BUNDLE_DIR);
        assert!(
            ignore.lines().any(|l| l.trim() == entry),
            "`.gitignore` must carry `{entry}` — the directory `render okf` writes \
             without `--out`"
        );
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
        // Neither `--fetch` nor `--okf` alters the pre-commit gate.
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

    /// The whole-file intent-debt opt-out the skill carries, spelled in two
    /// pieces on purpose.
    ///
    /// `scan_markers` matches this directive anywhere in a blob, so writing it
    /// contiguously *here* would exempt the whole of `init.rs` from intent-debt
    /// detection — silently, with nothing failing, and forever. `concat!` folds
    /// the two halves at compile time while keeping the source bytes from ever
    /// spelling the directive. (`markers.rs` and friends carry the real thing,
    /// deliberately, because they enumerate the vocabulary. This file does not.)
    const IGNORE_FILE_DIRECTIVE: &str = concat!("roteiro", ":ignore-file");

    /// The YAML frontmatter of `md`, or `None` if the document does not *begin*
    /// with a frontmatter block.
    ///
    /// Deliberately positional, and deliberately not a search: frontmatter is
    /// defined by being first, so the opening `---` must be at byte 0 and the
    /// block ends at the next `---` line. Anything before the opening delimiter
    /// — a comment, a blank line, a BOM — means the document has no
    /// frontmatter, and this returns `None` rather than hunting for a block
    /// further down. That mirrors what loaders actually do (an anchored
    /// `\A---\n(.*?)\n---\n`), which is the whole point: a guard that is more
    /// forgiving than the consumer cannot fail where the consumer does.
    fn frontmatter(md: &str) -> Option<&str> {
        let rest = md.strip_prefix("---\n")?;
        let end = rest.find("\n---\n")?;
        Some(&rest[..end])
    }

    #[test]
    fn skill_is_managed_and_a_valid_skill_document() {
        let md = skill_markdown();
        assert!(is_managed_skill(md), "skill carries the managed marker");
        assert!(!is_managed_skill("---\nname: other\n---\n"));

        // Portable SKILL.md contract: YAML frontmatter with name + description —
        // asserted by *position*, not by presence.
        //
        // This read `md.contains("name: roteiro")` for ten days (#596). `contains`
        // passes just as happily on a document whose block sits at line 3 behind
        // two HTML comments, at line 30, or inside a code fence — so it passed on
        // the one shipped by v2.0.0, which no harness could load. The frontmatter
        // was present and well-formed; it simply was not first, and being first is
        // the entire property that makes frontmatter frontmatter. A guard weaker
        // than the contract it names is why ten days passed with the defect in
        // every `roteiro init` run. Keep these assertions against `fm`, never `md`.
        let fm = frontmatter(md).unwrap_or_else(|| {
            panic!(
                "SKILL.md must *begin* with its YAML frontmatter: byte 0 must be the \
                 opening `---` line, and the block must close on a later `---` line \
                 before any other content. Nothing may precede the opening delimiter \
                 — not an HTML comment, not a blank line. A loader anchors its match \
                 at the start of the file and reports the frontmatter *missing* even \
                 when it is present and valid further down. Move the prose below the \
                 closing `---`; both markers this file carries are found by \
                 whole-document search, so neither needs to be first. The document \
                 currently begins:\n{}",
                md.lines().take(3).collect::<Vec<_>>().join("\n"),
            )
        });
        assert!(
            fm.contains("name: roteiro"),
            "has a skill name, inside the frontmatter block"
        );
        assert!(
            fm.contains("description:"),
            "has a description for relevance, inside the frontmatter block"
        );
        // The two markers that were moved below the frontmatter to make room for
        // it. Both are position-independent by construction — `is_managed_skill`
        // is a whole-document `contains`, and the whole-file debt opt-out is
        // matched over the whole blob — but "present at all" is the part that a
        // careless reorder can still destroy, and this file has lost content that
        // way before (#290, #377).
        assert!(
            md.contains(IGNORE_FILE_DIRECTIVE),
            "the skill enumerates the intent-debt vocabulary, so it must keep the \
             whole-file opt-out or it registers as debt itself"
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

    /// Pins the discriminating power of [`frontmatter`] itself, so the guard
    /// above cannot be quietly loosened back into a `contains`.
    ///
    /// The middle case is the exact document v2.0.0 shipped: a well-formed
    /// block with `name` and `description`, two HTML comments above it. Every
    /// needle the old assertion looked for is present in it. Only position
    /// separates it from the first case, so only a positional check can tell
    /// them apart.
    #[test]
    fn frontmatter_is_recognised_only_when_it_is_first() {
        let body = "name: roteiro\ndescription: d";
        let good = format!("---\n{body}\n---\n\n# Heading\n");
        assert_eq!(frontmatter(&good), Some(body), "block at byte 0 is read");

        let shipped = format!("<!-- roteiro-managed -->\n<!-- x -->\n---\n{body}\n---\n");
        assert!(
            shipped.contains("name: roteiro") && shipped.contains("description:"),
            "the shipped shape satisfies every needle the old guard checked"
        );
        assert_eq!(
            frontmatter(&shipped),
            None,
            "two comments above the block mean the document has no frontmatter"
        );

        // Even one blank line ahead of it is fatal to an anchored matcher.
        assert_eq!(frontmatter(&format!("\n---\n{body}\n---\n")), None);
        // An opening delimiter with no closing one is not a block either.
        assert_eq!(frontmatter("---\nname: roteiro\n"), None);
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
            //
            // `split('\n')`, not `lines()`: `lines()` strips a trailing `\r` and
            // drops the empty segment after a final newline, so a pure CRLF-vs-LF
            // divergence — one copy normalised by a Windows editor while the asset
            // stays LF — compares *equal* here even though the raw strings did not.
            // The search then finds nothing, the index falls off the end of both
            // sides, and the panic names a line that does not exist with `None` on
            // both sides: the diagnostic fails in precisely the case it exists for,
            // where the difference is invisible to the eye. `split` is lossless —
            // joining its segments with `\n` reconstructs the input byte for byte —
            // so unequal strings always diverge here, either at a segment or in
            // segment count, and `{:?}` renders the surviving `\r`.
            let (got, want): (Vec<_>, Vec<_>) = (
                committed.split('\n').collect(),
                template.split('\n').collect(),
            );
            // With a lossless split, no match means one side is a strict prefix of
            // the other, so this index is in range on the longer side and `None`
            // there reads as "this file ends here". It can no longer be `None` on
            // both: that would mean equal segment counts and no differing segment,
            // i.e. equal strings, which returned above.
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
                 `roteiro init` to regenerate both copies. If the two sides above read \
                 identically, compare them as printed: a trailing `\\r`, or a trailing \
                 empty segment, is a line-ending divergence rather than a content edit — \
                 there is nothing to move, and normalising this file to LF is the fix.",
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

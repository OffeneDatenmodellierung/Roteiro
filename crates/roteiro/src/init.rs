//! `roteiro init`: scaffold the graph and install the automation that keeps it
//! fresh — `post-checkout` / `post-merge` git hooks and an `AGENTS.md` snippet.
//!
//! Everything written here is *managed*: each artifact carries a marker so a
//! re-run updates Roteiro's own content in place and never clobbers a user's
//! existing (foreign) hook or notes.

use std::fs;
use std::io;
use std::path::Path;

/// Marker identifying a Roteiro-managed git hook.
const HOOK_MARKER: &str = "roteiro-managed";
/// Marker identifying the Roteiro block in `AGENTS.md`.
const AGENTS_MARKER: &str = "<!-- roteiro-managed -->";

/// The git hooks Roteiro installs to refresh the graph after `HEAD` moves.
pub const MANAGED_HOOKS: &[&str] = &["post-checkout", "post-merge"];

/// The content of a managed hook. Guards on `roteiro` being installed and never
/// fails the git operation, so it is safe on machines without the tool.
#[must_use]
pub fn hook_script() -> String {
    format!(
        "#!/bin/sh\n\
         # {HOOK_MARKER}: keep the Roteiro knowledge graph fresh after HEAD changes.\n\
         # Delete this file to disable. Re-run `roteiro init` to reinstall.\n\
         command -v roteiro >/dev/null 2>&1 && roteiro sync --committed >/dev/null 2>&1 || true\n"
    )
}

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
/// foreign hook of the same name.
///
/// # Errors
/// Returns [`io::Error`] on filesystem failure.
pub fn install_hook(hooks_dir: &Path, name: &str) -> io::Result<HookOutcome> {
    fs::create_dir_all(hooks_dir)?;
    let path = hooks_dir.join(name);
    let outcome = match fs::read_to_string(&path) {
        Ok(existing) if is_managed_hook(&existing) => HookOutcome::Updated,
        Ok(_) => return Ok(HookOutcome::SkippedForeign),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HookOutcome::Installed,
        Err(e) => return Err(e),
    };
    fs::write(&path, hook_script())?;
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
         This repository has a Roteiro knowledge graph — code structure, ADR intent,\n\
         and their links in one provenance-tagged store. Prefer querying it over\n\
         grepping when orienting:\n\
         \n\
         - `roteiro query <key> --json` — a node and its provenance-labelled edges.\n\
         Keys: `sym:<lang>:<path>#<Name>`, `file:<path>`, `adr:<id>`.\n\
         - `roteiro query --kind <kind> --json` — list nodes of a kind (`fn`, `adr`, …).\n\
         - `roteiro sync` — refresh the graph (git hooks do this automatically).\n\
         - `roteiro check` — validate ADR/annotation drift.\n\
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

#[cfg(test)]
mod tests {
    use super::{
        HookOutcome, agents_section, ensure_agents, hook_script, install_hook, is_managed_hook,
    };

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("roteiro-init-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn hook_is_recognisable_and_self_guarding() {
        let s = hook_script();
        assert!(is_managed_hook(&s));
        assert!(s.starts_with("#!/bin/sh"));
        assert!(s.contains("command -v roteiro"));
        assert!(!is_managed_hook("#!/bin/sh\necho other\n"));
    }

    #[test]
    fn install_creates_updates_and_skips_foreign() {
        let dir = tmp("hooks");
        let hooks = dir.join("hooks");

        // First install → created.
        assert_eq!(
            install_hook(&hooks, "post-checkout").expect("install"),
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
            install_hook(&hooks, "post-checkout").expect("reinstall"),
            HookOutcome::Updated
        );

        // A foreign hook is left untouched.
        let foreign = hooks.join("post-merge");
        std::fs::write(&foreign, "#!/bin/sh\necho mine\n").unwrap();
        assert_eq!(
            install_hook(&hooks, "post-merge").expect("skip"),
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
}

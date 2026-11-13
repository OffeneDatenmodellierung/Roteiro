//! End-to-end coverage for `roteiro docs prose` (ADR-0031 §5), driving the real
//! binary: it reports with the file's own line numbers, skips frontmatter, and
//! never fails.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

#[test]
fn reports_by_line_skips_frontmatter_and_always_succeeds() {
    let root = std::env::temp_dir().join(format!("roteiro-docs-prose-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("docs")).expect("mkdir");
    std::fs::write(
        root.join("docs/ADR.md"),
        "---\ntitle: We utilize frontmatter\n---\n\n# Heading\n\nThe store is small.\n\nWe utilize the cache.\n",
    )
    .expect("write");

    let out = Command::new(BIN)
        .args(["docs", "prose"])
        .current_dir(&root)
        // An isolated home, as `docs_fmt_cli.rs` does: user configuration is
        // loaded before dispatch.
        .env("ROTEIRO_HOME", &root)
        .env("HOME", &root)
        .output()
        .expect("run roteiro docs prose");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(out.status.success(), "a report, never a gate: {out:?}");
    assert!(
        text.contains("docs/ADR.md:9: unapproved-word — \"utilize\" — use \"use\""),
        "the issue carries the file's own line number:\n{text}"
    );
    assert!(!text.contains(":2:"), "frontmatter is not prose:\n{text}");
    assert!(
        text.contains("docs/ADR.md: 50/100 over 2 sentence(s)"),
        "{text}"
    );
    assert!(text.contains("nothing gates on it"), "{text}");

    std::fs::remove_dir_all(&root).ok();
}

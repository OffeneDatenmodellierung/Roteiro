//! End-to-end test for `roteiro review` (Stage 17): the CLI-first, graph-grounded
//! review of the working-tree change — it surfaces each touched symbol's
//! callers/callees and governing ADRs, and fails when the change introduces
//! authored-layer drift.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_roteiro");

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn roteiro(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run roteiro")
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("roteiro-review-cli-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

const ADR: &str = "---\n\
                   adr-id: \"0001\"\n\
                   status: Accepted\n\
                   ---\n\
                   \n\
                   # ADR-0001\n\
                   \n\
                   ## Decision\n\
                   \n\
                   The design centres on [[src/main.rs#greet]].\n";

#[test]
fn review_shows_context_for_a_clean_change_and_fails_on_drift() {
    let dir = fresh_dir("context");
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() {}\n",
    );
    write(&dir, "docs/adr/0001.md", ADR);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // No change yet → nothing to review.
    let empty = roteiro(&dir, &["review"]);
    assert!(empty.status.success());
    assert!(
        String::from_utf8_lossy(&empty.stdout).contains("no working-tree changes"),
        "clean tree reports nothing to review"
    );

    // A non-drift edit: `greet` gains a callee. Review passes and surfaces the
    // governing ADR and the caller/callee context.
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() { helper(); }\nfn helper() {}\n",
    );
    let clean = roteiro(&dir, &["review"]);
    let out = String::from_utf8_lossy(&clean.stdout);
    assert!(
        clean.status.success(),
        "non-drift review should exit 0: {out}"
    );
    assert!(
        out.contains("governed by: adr:0001#decision"),
        "shows the ADR: {out}"
    );
    assert!(
        out.contains("calls: sym:rust:src/main.rs#helper"),
        "shows callee: {out}"
    );
    assert!(
        out.contains("no authored-layer drift"),
        "no drift reported: {out}"
    );

    // A drift-introducing edit: rename `greet`, so the ADR's link dangles. Review
    // reports the drift and exits non-zero.
    write(
        &dir,
        "src/main.rs",
        "fn main() { hello(); }\nfn hello() {}\n",
    );
    let drift = roteiro(&dir, &["review"]);
    let out = String::from_utf8_lossy(&drift.stdout);
    assert!(
        !drift.status.success(),
        "drift review must exit non-zero: {out}"
    );
    assert!(
        out.contains("drift introduced by this change"),
        "reports drift: {out}"
    );
    assert!(
        out.contains("src/main.rs#greet"),
        "names the dangling link target: {out}"
    );

    // JSON carries the schema and the drift.
    let json = roteiro(&dir, &["review", "--json"]);
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(
        text.contains("\"schema\": \"roteiro.review/v1\""),
        "schema tag: {text}"
    );
    assert!(text.contains("\"drift\""), "drift field present: {text}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn review_detects_drift_from_editing_an_adr() {
    // Regression: a broken link introduced by editing the *ADR* file. Its
    // violation message leads with the ADR node key (`adr:0001#…`), not the ADR
    // path, so drift attribution must resolve that key's node path.
    let dir = fresh_dir("adr-edit");
    write(
        &dir,
        "src/main.rs",
        "fn main() { greet(); }\nfn greet() {}\n",
    );
    write(&dir, "docs/adr/0001.md", ADR);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    assert!(roteiro(&dir, &["sync"]).status.success(), "initial sync");

    // Edit only the ADR: add a link to a symbol that does not exist.
    let edited = format!("{ADR}\nAnd also [[src/main.rs#missing]].\n");
    write(&dir, "docs/adr/0001.md", &edited);

    let out = roteiro(&dir, &["review"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "editing an ADR to add a dangling link must be caught as drift: {text}"
    );
    assert!(
        text.contains("drift introduced by this change"),
        "reports the ADR-edit drift: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

# Polyglot analyzer fixture tree

Five files, one per language the coverage requirement names, each containing a
construct the vendored baseline rule set
(`crates/rto-exec/rules/roteiro-baseline.yml`) matches. This tree exists so
"semgrep produces findings for Rust, Python, SQL, Java and Node" is a thing the
test suite checks rather than a claim in a document.

**Nothing here is real code and none of it is compiled, imported or executed.**
Every file is deliberately unsafe — that is the point of a fixture for a
security analyzer — and the tree is excluded from the workspace so `cargo build`
never sees it.

Two kinds of test use it:

- `tests/polyglot.rs` runs semgrep over the tree **when a semgrep binary is on
  `PATH`**, and asserts at least one finding per language. CI has no semgrep, so
  that test self-skips with a visible message.
- `native/semgrep-polyglot.json` is the output of exactly that run, captured
  once and committed. The fixture-driven tests normalise it with no tool
  present, which is what actually runs in CI.

To refresh the captured output after changing a rule or a fixture:

```sh
cd crates/rto-exec/tests/fixtures/polyglot
semgrep scan --json --quiet --metrics=off --disable-version-check \
  --config ../../../rules/roteiro-baseline.yml . \
  > ../native/semgrep-polyglot.json
```

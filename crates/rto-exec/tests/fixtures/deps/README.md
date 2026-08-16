# Dependency-manifest fixture tree

Four lockfiles, one per ecosystem `osv-scanner` closes the dependency axis for
(ADR-0018). This tree exists so "a Python, a Java and a Node lockfile each
produce findings" is a thing the test suite checks rather than a claim in a
document — the same job `../polyglot/` does for the SAST axis.

| File | Ecosystem | Pins | Why this one |
|---|---|---|---|
| `python/requirements.txt` | PyPI | `requests 2.19.1` | Several PYSEC advisories; a plain `requirements.txt` needs no resolver and so no network. |
| `java/gradle.lockfile` | Maven | `org.apache.logging.log4j:log4j-core 2.14.1` | Log4Shell and its neighbours. A `gradle.lockfile` is already resolved, unlike a `pom.xml`, which would need a Maven registry. |
| `node/package-lock.json` | npm | `minimist 1.2.0` | Two GHSA advisories, and a lockfile format `osv-scanner` reads directly. |
| `rust/Cargo.lock` | crates.io | `time 0.2.22`, `derivative 2.2.0` | The two cases the Rust overlap needs: a real vulnerability that `cargo-audit` also reports (`RUSTSEC-2020-0071`, aliased to `CVE-2020-26235` and `GHSA-wcg3-cvx6-7396`), and an *informational* advisory (`RUSTSEC-2024-0388`, `unmaintained`). |

**Nothing here is a real project.** No file is built, installed or resolved;
these are inert manifests naming versions that are known to be affected, which
is the point of a fixture for a dependency scanner. The tree is excluded from
the workspace, so `cargo build` never sees it.

## The captured output

`../native/osv-scanner-deps.json` is real `osv-scanner` output over exactly this
tree:

```
osv-scanner scan source --offline --local-db-path <pinned> --format json --recursive .
```

- **osv-scanner 2.5.0** (osv-scalibr 0.4.5), fully offline against pinned
  per-ecosystem OSV databases downloaded on **2026-08-16** (the `crates.io`
  snapshot was published `2026-08-15T05:33:51Z`).
- Exit status `1` — which for this tool means *vulnerabilities found*, not
  *failure*. See the adapter's `success_statuses`.

**One substitution was made, and only one.** `osv-scanner` reports **absolute**
paths even when told to scan `.`, so the capture named the machine it ran on.
The four `results[].source.path` values had that prefix replaced with
`/checkout`; nothing else in the file was touched. A developer's home directory
has no business in a committed fixture — which is the same reason the adapter
makes those paths worktree-relative before they reach a finding key, and the
tests point it at `/checkout` to exercise exactly that.

Because the databases move daily, a re-capture will legitimately differ: the
fixture-driven tests therefore assert *properties* (every ecosystem yields
findings; a duplicated advisory collapses to one; paths come out relative) and
never an advisory count.

---
Title: Analyzer coverage — which analyzers deliver which languages, and on which axis
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0018"
status: For Review                  # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Security Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-15
confluence-url:
---

# ADR-0018: Analyzer coverage — which analyzers deliver which languages, and on which axis

| | |
|---|---|
| **State** | For Review |
| **Architectural Significance** | MEDIUM |
| **Domain** | Security Tooling |
| **Document version** | 1.0 |

## Reference

Decides **which analyzers Roteiro ships adapters for**, and records the evidence
for the coverage each one actually delivers. Its siblings decide the surrounding
questions: [[docs/adr/0012-analyzer-findings-artifact-model.md]] how findings are
stored, [[docs/adr/0014-sandboxed-analyzer-execution.md]] how analyzers are
executed and provisioned. This ADR exists because those two name `cargo-audit`
and `semgrep` as *examples* — "the two analyzers named above are therefore
examples, not schema" — and the question of which analyzers actually cover the
project's languages was left open.

## Summary

- **Two axes, not one.** SAST (patterns in source) and dependency
  vulnerabilities (advisories against a resolved manifest) are different
  questions with different tools, different output formats, and different
  staleness behaviour. A matrix that conflates them will always look more
  complete than it is.
- **semgrep** delivers the SAST axis for all five required languages, with SQL
  qualified: semgrep has **no SQL language support at any maturity level**, so
  SQL is matched by its `generic` (token) engine.
- **cargo-audit** delivers the dependency axis for **Rust only**. Python, Java
  and Node dependency vulnerabilities are **not covered by this stage**.
- **`osv-scanner` is the recommended next analyzer** and closes the dependency
  axis for Python, Java and Node in one tool with one output format — assessed
  here, sequenced separately.
- Rules are **vendored and pinned, not fetched**, and are the repository's own:
  Semgrep Registry rules carry the *Semgrep Rules License v1.0*, which is not on
  `deny.toml`'s allow-list — and `cargo deny` governs crates, so it would never
  have caught a rule file.

## Context

The coverage requirement is that findings are produced for **Rust, Python, SQL,
Java and Node**. `cargo-audit` and `semgrep` were named in the plan, and neither
document established that the pair actually delivers that.

Two things had to be checked rather than assumed.

**Whether one tool can cover five languages.** Only a SAST tool has any chance;
dependency scanners are ecosystem-specific by construction.

**Whether "coverage" means the same thing for each language.** It does not.
"Findings are produced for SQL" is true of semgrep's generic engine and would
also be true of a `grep` for the word `GRANT`. Recording *how* each language is
covered is the difference between a matrix and a marketing claim.

### The evidence

**Semgrep's supported languages** (`semgrep.dev/docs/supported-languages`,
fetched 2026-08-15). Semgrep Code lists Rust, Java, JavaScript, Typescript and
Python all as **Generally available**. `Generic` is also Generally available.
**SQL does not appear on the page at any maturity level** — not GA, not beta, not
in the experimental list (Bash, Cairo, Circom, Clojure, Dockerfile, Hack, HTML,
Jsonnet, Julia, Lisp, Lua, Move, OCaml, R, Scheme, Solidity, YAML, XML). This was
the specific thing worth checking, and it came back negative.

**Semgrep's dependency scanning (Supply Chain) is not the OSS CLI.** The same
page lists 14 languages for Supply Chain with reachability analysis; that is a
hosted product feature, not something `semgrep scan` does against a lockfile.
Semgrep therefore contributes nothing to the dependency axis here.

**`cargo audit` reads `Cargo.lock` and nothing else** — Rust only, by
construction.

**`osv-scanner`** (Apache-2.0, google/osv-scanner) states support for "C/C++,
Dart, Elixir, Go, Java, Javascript, PHP, Python, R, Ruby, Rust" across "npm, pip,
yarn, maven, go modules, cargo, gem, composer, nuget and others", backed by
OSV.dev — which itself ingests the RustSec advisory database and GitHub Security
Advisories. It has a documented **offline mode** with per-ecosystem databases at
`https://osv-vulnerabilities.storage.googleapis.com/<ECOSYSTEM>/all.zip`: single
files, stable URLs, digest-pinnable — which is a materially better provisioning
story than `cargo-audit`'s git checkout.

### The matrix

| Language | SAST | Dependency vulnerabilities |
|---|---|---|
| Rust | **semgrep** (GA) | **cargo-audit** (RustSec) |
| Python | **semgrep** (GA) | *not covered* → `osv-scanner` |
| Java | **semgrep** (GA) | *not covered* → `osv-scanner` |
| Node (JS/TS) | **semgrep** (GA) | *not covered* → `osv-scanner` |
| SQL | **semgrep `generic`** — token matching, no parser | n/a — SQL has no dependency ecosystem |

The five-language requirement is met **on the SAST axis**. On the dependency
axis, only Rust is covered, and the table says so rather than leaving a reader to
infer it from a tool list.

## Decision makers

The Roteiro Project Team.

## Recommended option

**Ship `semgrep` and `cargo-audit` adapters now; add `osv-scanner` next.**

### Why this set, and not a larger one

The alternative was four ecosystem-specific dependency scanners — `pip-audit`,
`npm audit`, OWASP Dependency-Check, `cargo-audit` — which is four output
formats, four provisioning stories, four staleness models and four upstreams to
track. `osv-scanner` replaces three of them with one tool, one format and one
offline database mechanism. Adding it *instead of* the three is the smaller
change as well as the better one, which is why the three are not being written.

`cargo-audit` is kept even though `osv-scanner` also reads `Cargo.lock`, because
it reports RustSec's informational kinds — `unmaintained`, `unsound`, `yanked` —
that a pure vulnerability database does not carry. `deny.toml` in this repository
already ignores two `unmaintained` advisories by id, so those findings are
demonstrably load-bearing here.

### SQL is a qualified answer, deliberately

Semgrep's `generic` mode is a token matcher: no AST, no dataflow, no types. It
can say *this statement grants ALL PRIVILEGES*. It cannot say *this value reaches
a query unsanitised* — the finding people actually want from SQL analysis. The
qualification is carried in three places so it cannot be lost: the adapter's
declared language list reads `sql (generic mode)`, every SQL rule carries an
`engine-note` in its metadata, and a test asserts that note is present on every
SQL finding.

A SQL-parsing analyzer (`sqlfluff`, `sqlcheck`) would be a genuine improvement
and is out of scope here.

### Rules are vendored, pinned, and ours

`semgrep --config p/default` resolves against the Semgrep Registry, which is a
**network service**. An analyzer that calls a registry per run is not
offline-capable and is not reproducible: the same commit yields different
findings as the registry moves. The shipped rule set is a local file,
digest-pinned, installed by `roteiro security prefetch`, and stamped onto every
run as `rules_digest`.

**Licence position, stated because no gate would catch it.** Every shipped rule
was written for this repository and carries the repository's own licence (MIT OR
Apache-2.0). **No Semgrep Registry rule is vendored or copied**, including from
the Community Edition set in `semgrep/semgrep-rules`, whose `LICENSE` reads
"Semgrep Rules License v1.0" — not an SPDX identifier on `deny.toml`'s
allow-list. `cargo deny` checks crates; a YAML file of rules would have sailed
past it. Semgrep the tool is LGPL-2.1 and is invoked as a separate process: never
linked, never vendored, never redistributed by Roteiro.

Operators who want broader coverage pin their own rule set. The machinery does
not care how many rules there are; the baseline exists to prove the pipeline, not
to be an audit.

### `prefetch` verifies and pins; as shipped, it fetches nothing

[[docs/adr/0014-sandboxed-analyzer-execution.md]] describes `roteiro security
prefetch` as "fetch and verify all pinned assets by digest". As implemented for
this stage it **verifies and pins, and fetches nothing at all**. That is a
deviation from the wording, recorded here rather than discovered later.

Neither shipped asset is fetchable in the sense that wording implies:

- **The semgrep rule set is vendored into the binary.** There is nothing to
  fetch: `prefetch` writes the compiled-in bytes to the cache, digests them, and
  records the result. This is a feature, not a shortcut — a fresh machine with no
  network can provision and then scan, which is the case ADR-0014's "mostly
  offline" model exists to serve.
- **The `RustSec` advisory database is a git checkout**, not a file with a
  digest-stable URL. GitHub's generated tarballs are not byte-stable over time,
  so a `sha256` pin against one would break on a re-gzip that changed no advisory.
  Rather than shell out to `git` — which is precisely the "silent fall back to
  host tools" ADR-0014 forbids — `prefetch` verifies the directory is there,
  digests its contents, records the digest and the publication date, and refuses
  with the exact clone command when it is absent.

The important half of the contract is unaffected and is arguably *strengthened*:
a run consults inputs whose identity was pinned before it started, nothing is
ever fetched implicitly, and no code path falls back to whatever the host
happens to have installed. What is deferred is only the fetching.

**The first genuinely downloadable asset arrives with `osv-scanner` in Stage
22b.** Its per-ecosystem databases
(`https://osv-vulnerabilities.storage.googleapis.com/<ECOSYSTEM>/all.zip`) are
single files at stable URLs — exactly what a digest pin wants. `AssetSource` is
`#[non_exhaustive]` so that source can be added without a breaking change, and
ADR-0014's wording becomes literally true at that point rather than
aspirationally true now.

### Three things the tools do that their documentation does not say

All three were found by running the tools, and each would have been a silent
defect:

1. **Semgrep rewrites rule ids with the config's filesystem path.** With
   `--config <file>`, rule `roteiro.python.eval-of-input` is reported as
   `<path.to.config.dir>.roteiro.python.eval-of-input`. Since the rule id is the
   first component of a `FindingKey`, that would have put the user's asset-cache
   directory into every stored key — user-identifying data in a persisted record,
   and keys that differ between two machines running the identical scan.
   `--no-rewrite-rule-ids` is mandatory, not cosmetic.
2. **Semgrep redacts the matched source.** In the open-source CLI, `extra.lines`
   and `extra.fingerprint` are the literal string `"requires login"` unless the
   caller is authenticated to Semgrep's hosted platform. ADR-0012's identity
   recipe ends in a snippet hash, so hashing that field would have made the
   component a constant — and changed every stored key the day somebody logged
   in. The snippet is read from the worktree instead, which is a function of the
   source rather than of the analyzer's authentication state.
3. **`cargo audit` reports no advisory-database identity when you pin one.**
   Given `--db <path>` it returns `last-commit: null` and `last-updated: null` —
   at a shallow clone and at its own managed checkout alike (0.22.2). Only the
   unpinned, self-resolving configuration populates them. So the reproducible,
   offline, pinned configuration was the one that lost its staleness evidence.
   Provisioning records the database's publication date from its `HEAD` commit
   time, and the adapter uses that when the report says nothing; the tool's own
   account still wins where it has one.

### Severity is a mapping where the tool does not publish one

RustSec publishes no qualitative severity — only a CVSS **vector** on some
advisories and an `informational` kind on others. Roteiro maps the kind
(`vulnerability` → high, `unsound` → medium, `unmaintained`/`yanked` → low,
`notice` → info) and preserves the raw CVSS vector, aliases, `related` CVEs and
categories verbatim in the finding's `meta`. Computing a base score from the
vector is deliberately not done: it is a versioned scoring algorithm, and a
number that disagreed with `cargo audit`'s own would be worse than carrying the
vector unchanged.

## Options considered + consequences

| Option | Verdict |
|---|---|
| semgrep alone | **Rejected** — covers five languages on the SAST axis and zero on the dependency axis, while looking like full coverage. |
| semgrep + four ecosystem dependency scanners | **Rejected** — four output formats, four provisioning stories, four upstreams; `osv-scanner` replaces three of them. |
| semgrep + `osv-scanner` only | **Rejected as the whole answer** — `osv-scanner` does not report RustSec's `unmaintained`/`unsound`/`yanked` kinds, which this repository already relies on. |
| **semgrep + cargo-audit now, `osv-scanner` next (chosen)** | Five languages covered on SAST immediately; the dependency axis closed by one further tool rather than three. |
| Semgrep Registry rules, vendored | **Rejected** — Semgrep Rules License v1.0 is not on the `deny.toml` allow-list, and no existing gate would have caught it. |
| Semgrep Registry rules, fetched per run | **Rejected** — a network service per run is neither offline-capable nor reproducible. |
| A SQL-parsing analyzer for the SQL axis | **Deferred** — a real improvement over generic matching, and a separate decision. |

## Consequences

**Positive**

- The coverage claim is a table with evidence behind each cell, and a test that
  fails if a language stops producing findings.
- One SAST tool covers four languages properly and the fifth with a stated
  limitation, rather than five tools covering five languages unevenly.
- The rule-licence question is answered in writing, where no automated gate
  reaches.
- Three real tool behaviours are recorded, so the next person does not rediscover
  them by shipping a defect.

**Negative / costs**

- **Python, Java and Node dependency vulnerabilities are not covered.** This is
  the headline gap, and it is a gap until `osv-scanner` lands.
- SQL coverage is token matching, and always reads as weaker than the other four
  because it is.
- The baseline rule set is small by design, so a clean scan means little until an
  operator pins a real one.
- `cargo-audit` and `osv-scanner` will overlap on Rust once both ship, and the
  duplicate-finding question is left to that change.

## Status

For Review. The semgrep and `cargo-audit` adapters, the subprocess runner and the
`prefetch`/`status` provisioning land in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md)
Stage 22. **`osv-scanner` is the recommended immediate follow-up** and is the
change that makes the dependency axis match the SAST axis; it reuses the adapter
seam, the subprocess runner and the asset cache unchanged, and its per-ecosystem
`all.zip` databases are the first asset that genuinely wants a download-by-URL
source (`AssetSource` is `#[non_exhaustive]` for exactly that reason).

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-08-15 | Initial: the analyzer→language matrix with evidence, the SQL qualification, the rule-licence position, and the three verified tool behaviours. |

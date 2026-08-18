---
Title: Analyzer coverage — which analyzers deliver which languages, and on which axis
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0018"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Security Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.3"
last-modified: 2026-08-16
confluence-url:
---

# ADR-0018: Analyzer coverage — which analyzers deliver which languages, and on which axis

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Security Tooling |
| **Document version** | 1.3 |

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
- **cargo-audit** delivers the dependency axis for **Rust only**.
- **`osv-scanner` closes the dependency axis for Python, Java and Node** in one
  tool with one output format, against OSV.dev's per-ecosystem databases. Shipped
  in Stage 22b (v1.2); assessed here before it was written.
- **The two overlap on Rust, and both are kept**, cross-referenced at the
  reporting layer on identifiers both upstreams publish (v1.1).
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
| Rust | **semgrep** (GA) | **cargo-audit** (`RustSec`) **+ `osv-scanner`** (OSV `crates.io`) — both kept, cross-referenced |
| Python | **semgrep** (GA) | **`osv-scanner`** (OSV `PyPI`) |
| Java | **semgrep** (GA) | **`osv-scanner`** (OSV `Maven`) |
| Node (JS/TS) | **semgrep** (GA) | **`osv-scanner`** (OSV `npm`) |
| SQL | **semgrep `generic`** — token matching, no parser | n/a — SQL has no dependency ecosystem |

Both axes are now covered for every language that has one. As of v1.0 the
dependency column read *not covered* for three of the five rows; Stage 22b filled
them, and every cell above is pinned by a test over real analyzer output rather
than by this table.

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

`cargo-audit` is kept even though `osv-scanner` also reads `Cargo.lock`.
**v1.0 gave the wrong reason for this and v1.2 corrects it**: the claim was that
`osv-scanner` does not report `RustSec`'s informational kinds, and measurement
shows it reports two of the three. The reason that survives is narrower and
firmer — **`yanked` is not an advisory at all.** `cargo audit` learns it from the
crates.io registry index, so no OSV consumer can ever carry it. `deny.toml` in
this repository already ignores two `unmaintained` advisories by id, so
informational findings are demonstrably load-bearing here; that they now arrive
from *both* tools is what the cross-reference exists to render.

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

**The first genuinely downloadable asset arrived with `osv-scanner` in Stage
22b (v1.2).** Its per-ecosystem databases
(`https://osv-vulnerabilities.storage.googleapis.com/<ECOSYSTEM>/all.zip`) are
single files at stable URLs — exactly what a digest pin wants. `AssetSource` was
`#[non_exhaustive]` so that source could be added without a breaking change, and
ADR-0014's wording is now literally true rather than aspirationally so.

Three things were held constant while adding it:

- **A run still never fetches.** The transport is not in `rto-exec` at all: the
  provisioning function takes the fetcher as an argument, and the resolution path
  a run uses has none to pass. "Provisioning writes, running reads" is a property
  of the signatures rather than a rule to remember.
- **Downloading is asked for, not implied.** `prefetch` needs `--allow-download`,
  and prints every URL before opening a socket. The four databases are roughly
  **260 MB** (`npm/all.zip` alone is about 210 MB), which is not a reasonable
  surprise for a command people run when unsure.
- **The pin is the provisioned snapshot, not a compile-time digest.** OSV
  rebuilds these files daily, so a hard-coded `sha256` would be wrong within a
  day. What is recorded is the digest of what *this machine* provisioned, and a
  run is refused if the bytes have moved since — the same pin the `RustSec`
  checkout gets, and the only one that can actually be honoured.

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

### Three more things `osv-scanner` does that its documentation does not say

Found the same way — by running it — and each would have been a silent defect.
Measured against **osv-scanner 2.5.0**.

4. **`--offline-vulnerabilities` on its own consults no local database.** Its
   help text reads "checks for vulnerabilities using local databases that are
   already cached", but with only that flag the scanner reports **zero findings
   and exits `0`**, even with the database sitting in the very cache it names.
   The database is loaded only under `--offline` (or
   `--download-offline-databases`). A clean bill of health produced by reading
   nothing is the worst failure mode a security tool has, and the adapter passes
   `--offline` for exactly that reason rather than as a matter of style. The
   companion behaviour is the reassuring one: `--offline` with **no** database
   present exits `127` with "no offline version of the OSV database is
   available", and `127` is not a declared success status, so a failed scan
   fails.
5. **Reported paths are absolute even when the scan target is `.`.** Given
   `scan source --recursive .` with the working directory set to the worktree,
   every `results[].source.path` still comes back as a full filesystem path.
   Since the manifest is part of the finding's identity, storing it verbatim
   would put the scanning machine's home directory into a persisted record —
   the same defect as semgrep's rule-id rewriting (1), reached by a different
   route. The adapter makes the path worktree-relative, and records the location
   as unknown rather than guessing when it cannot.
6. **The same advisory is listed twice, and the tool already knows.**
   `vulnerabilities` contains both the `RUSTSEC` record and its `GHSA` twin as
   separate entries, while `groups` says which ids are one advisory. Converting
   per `vulnerabilities` entry would double-count every advisory GitHub has also
   assigned a GHSA id to — a count wrong in the direction that looks like more
   work than there is. The adapter emits one finding per **group**.

### Severity is a mapping where the tool does not publish one

RustSec publishes no qualitative severity — only a CVSS **vector** on some
advisories and an `informational` kind on others. Roteiro maps the kind
(`vulnerability` → high, `unsound` → medium, `unmaintained`/`yanked` → low,
`notice` → info) and preserves the raw CVSS vector, aliases, `related` CVEs and
categories verbatim in the finding's `meta`. Computing a base score from the
vector is deliberately not done: it is a versioned scoring algorithm, and a
number that disagreed with `cargo audit`'s own would be worse than carrying the
vector unchanged.

### The Rust overlap between `cargo-audit` and `osv-scanner` (v1.1)

Once `osv-scanner` ships (Stage 22b) it will also read `Cargo.lock`, and OSV.dev
ingests the RustSec database — so **the same Rust advisory arrives twice**, under
`finding:osv-scanner:…` and `finding:cargo-audit:…`. The findings schema has no
notion of cross-analyzer identity: a layer is keyed
`security:<analyzer>:<worktree-id>` and replaced wholesale *per analyzer*, so
nothing dedupes them on the way in.

**Decision: keep both findings, and cross-reference them at the reporting layer.**
Neither analyzer's layer is filtered, trimmed or made conditional on the other.

**Why this is cheaper than it sounds — the join key already exists on both sides
and needs no invention.** OSV keys a RustSec-derived record by *the RUSTSEC id
itself*, so `GET /v1/vulns/RUSTSEC-2020-0071` resolves, and that record carries
`aliases: ["CVE-2020-26235", "GHSA-wcg3-cvx6-7396"]`. On the other side,
`cargo-audit`'s adapter already stores the advisory's `aliases` verbatim in
`meta` (`crates/rto-exec/src/adapter/cargo_audit.rs:225`). So the reporting layer
joins on the RUSTSEC id, falling back to **alias-set intersection** where the two
sides name the same advisory by different ids. That is a deterministic join over
identifiers both upstreams publish — not a similarity match, not a heuristic, and
nothing that needs a confidence score.

**Why not drop one side.** Suppressing `cargo-audit`'s vulnerability findings and
keeping only its informational kinds was considered and rejected: it assumes OSV
carries no `unmaintained`/`unsound`/`yanked`, and **that assumption is false at
the database level** — `RUSTSEC-2024-0388` (`derivative` is unmaintained),
`RUSTSEC-2021-0139` (`ansi_term` is Unmaintained) and `RUSTSEC-2026-0192`
(`ttf-parser` is unmaintained) all resolve in OSV, each with `aliases: null`.
Suppression also fails a subtler test: it makes each tool's output depend on
which *other* tools are installed, so a finding set stops being a property of the
tool and the tree. Two analyzers that independently agree are **evidence**, and
throwing one away to tidy a count discards it.

**The two things v1.1 asked Stage 22b to measure, now measured (v1.2).**

**1. The database is not the tool — and v1.0 conflated them.** The v1.0 options
table said *"`osv-scanner` does not report RustSec's
`unmaintained`/`unsound`/`yanked` kinds"*. Measured against **osv-scanner 2.5.0**
(osv-scalibr 0.4.5), fully offline against a pinned `crates.io` database, with
**no extra flags**:

| RustSec kind | Reported by `osv-scanner` by default? | Evidence |
|---|---|---|
| `unmaintained` | **Yes** | `RUSTSEC-2021-0139` (`ansi_term`) and `RUSTSEC-2024-0388` (`derivative`) both appear in a default-flag scan. |
| `unsound` | **Yes** | `RUSTSEC-2023-0072` (`openssl` `X509StoreRef::objects`) appears the same way. |
| `yanked` | **No, and it never can** | *Yanked* is not an advisory. `cargo audit` learns it from the crates.io registry index; there is no OSV record to carry, so no database snapshot can supply it. |

The kind travels through OSV as `affected[].database_specific.informational`, and
the adapter maps it with **the identical mapping `cargo-audit` uses**, so a
cross-referenced pair does not read as two severities for one advisory.

The `--all-vulns` flag ("show all vulnerabilities including unimportant and
uncalled ones") was checked in case the default was hiding something: over the
fixture tree it changed **nothing** — the same set of advisory groups either way.

**So v1.0's row was wrong in its stated reason, and right in its conclusion for a
different one.** `cargo-audit` is still kept, but not because it is the only
source of informational advisories — two of the three kinds arrive from OSV too.
It is kept because *yanked* is structurally unavailable to any OSV consumer, and
because two independent sources agreeing is evidence worth keeping.

**2. Ingestion lag is minutes, not days — but the *pin* is what actually
diverges.** Measured on 2026-08-16: `RUSTSEC-2026-0257` was assigned in the
`RustSec` repository at `2026-08-12T10:42:29Z`, and its OSV record's `modified`
stamp is `2026-08-12T10:45:03Z` — **about two and a half minutes**. RustSec→OSV
ingestion is therefore not a meaningful source of disagreement.

What does diverge is the **pin**. Each analyzer's database is provisioned
separately and held until the operator re-runs `prefetch`: `cargo-audit` reads a
git checkout, `osv-scanner` reads a daily-rebuilt `all.zip` snapshot. Two
machines, or one machine prefetched a week apart, will legitimately report
different sets. The reporting layer therefore renders *present in one* as a
normal single-source row with its cause named, never as a discrepancy — and the
`age_days`/`published_at` evidence already on every run is what a reader uses to
tell "the other tool has not caught up" from "the other tool disagrees".

**What "cross-reference" must not become.** Not a merged super-finding, and not a
count that silently halves. A duplicate pair should read as *one advisory,
confirmed by two analyzers*, with both finding keys still addressable — because
each layer is still replaced per analyzer, and a reader who fixes the advisory
must see both disappear.

## Options considered + consequences

| Option | Verdict |
|---|---|
| semgrep alone | **Rejected** — covers five languages on the SAST axis and zero on the dependency axis, while looking like full coverage. |
| semgrep + four ecosystem dependency scanners | **Rejected** — four output formats, four provisioning stories, four upstreams; `osv-scanner` replaces three of them. |
| semgrep + `osv-scanner` only | **Rejected as the whole answer, on a corrected reason (v1.2).** v1.0 said `osv-scanner` reports none of `RustSec`'s informational kinds; measurement shows it reports `unmaintained` and `unsound`. What it cannot report is **`yanked`**, which is not an advisory — `cargo audit` reads it from the crates.io registry index — so no OSV consumer can supply it. |
| **semgrep + cargo-audit now, `osv-scanner` next (chosen)** | Five languages covered on SAST immediately; the dependency axis closed by one further tool rather than three. **Delivered:** `osv-scanner` shipped in Stage 22b. |
| `osv-scanner`: one finding per `vulnerabilities` entry | **Rejected (v1.2)** — the tool lists a `RUSTSEC` record and its `GHSA` twin separately and resolves them itself in `groups`. Per-entry conversion doubles every dual-assigned advisory. One finding per group. |
| `osv-scanner`: pass `--all-vulns` | **Rejected (v1.2)** — measured to change nothing over the fixture tree, and the informational kinds it might have been needed for already arrive by default. An unnecessary flag is a behaviour nobody can explain later. |
| Rust overlap: keep both + cross-reference (chosen, v1.1) | Joins on the RUSTSEC id and alias sets, which both upstreams already publish. Agreement between two analyzers is kept as evidence. |
| Rust overlap: suppress `cargo-audit`'s vulnerability findings | **Rejected** — rests on OSV lacking RustSec's informational kinds, which is false at the database level; and it makes a tool's output depend on which other tools are installed. |
| Rust overlap: leave both, unlinked | **Rejected** — every Rust advisory then reads as two unrelated problems, and fixing one drops the count by two with no explanation. |
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

- ~~**Python, Java and Node dependency vulnerabilities are not covered.**~~
  **Closed in Stage 22b (v1.2).** `osv-scanner` covers all three, and a test over
  real output asserts each one still yields findings. What remains is the cost
  that replaced it: those ecosystems' OSV databases are about **260 MB** to
  provision, and they are only as current as the last `prefetch`.
- SQL coverage is token matching, and always reads as weaker than the other four
  because it is.
- The baseline rule set is small by design, so a clean scan means little until an
  operator pins a real one.
- `cargo-audit` and `osv-scanner` overlap on Rust. **Decided in v1.1, shipped in
  v1.2:** both are kept and cross-referenced at the reporting layer on
  identifiers both upstreams publish, so a duplicate pair renders as one advisory
  confirmed twice rather than as two problems, with both finding keys still
  addressable.
- **The join needed one constraint the decision did not state, and real data
  found it.** Identifier intersection alone over-merges: in this repository's own
  `cargo-audit` capture, `chrono`'s advisory lists `CVE-2020-26235` and
  `RUSTSEC-2020-0071` under `related` — the identifiers `time`'s advisory is
  published under. Correspondence therefore also requires the **same package at
  the same version**, and a correspondence is named only by an id an analyzer
  actually fired, never by one it merely aliased.
- **Two analyzers on one axis means two databases to keep current.** A reader now
  has to understand that *present in one, absent in the other* is ordinary — the
  pins are independent — and the reporting layer has to say so, because a bare
  asymmetry reads as a bug.

## Status

**Accepted** (2026-08-17), and implemented — Stages 22 and 22b (#322, #339), released in **v1.11.0** and **v1.11.x**. The semgrep and `cargo-audit` adapters, the subprocess runner and the
`prefetch`/`status` provisioning landed in [BUILD_PLAN_V2](../BUILD_PLAN_V2.md)
Stage 22. **`osv-scanner` landed in Stage 22b**, and with it the dependency axis
matches the SAST axis.

It reused the adapter seam, the subprocess runner, the asset cache and
`prefetch`/`status` unchanged — the seam doing its job — and needed **no
migration**: `FindingKey` already takes each analyzer's own ordered identity
components, and `RunnerKind` already names the backends. Two additions were
genuinely new: `AssetSource::Download`, the first asset fetched by URL, and a
cross-reference at the reporting layer for the Rust overlap.

## Version history

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-08-15 | Initial: the analyzer→language matrix with evidence, the SQL qualification, the rule-licence position, and the three verified tool behaviours. |
| 1.1 | 2026-08-16 | Resolves the Rust overlap left open by v1.0: keep both `cargo-audit` and `osv-scanner` findings and cross-reference them on the RUSTSEC id / alias set. Records that OSV.dev *does* carry `RustSec` informational advisories, which refutes the premise of the suppression option, and flags the database-vs-scanner distinction for Stage 22b to measure. |
| 1.2 | 2026-08-16 | `osv-scanner` shipped (Stage 22b); the dependency column of the matrix is filled. **Corrects v1.0's options-table row**, which conflated the database with the tool: measured against osv-scanner 2.5.0, the scanner *does* report `unmaintained` and `unsound` by default; only `yanked` is unavailable to it, and structurally so. Records the measured RustSec→OSV ingestion lag (~2.5 minutes, so pin age rather than ingestion is what makes the two analyzers differ), three further undocumented tool behaviours, and the package-and-version constraint the cross-reference join turned out to need. |
| 1.3 | 2026-08-17 | **Accepted.** No content changed. Status corrected: this ADR described shipped, released behaviour while still reading *For Review*. |

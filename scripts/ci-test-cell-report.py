#!/usr/bin/env python3
"""Which test binaries a `cargo test` cell actually ran — and which it never
reached.

Issue #906. A `cargo test` without `--no-fail-fast` stops at the first failing
test binary, so the later ones are never run. The job goes red, which looks like
honest reporting, but the *scope* it reports on is smaller than the scope it
appears to cover and nothing says so. On this repository one flaky test in
`crates/roteiro` silently truncated the `--all-features` cell for weeks, and
workers added `-p`-scoped runs beside the full cell without ever establishing
why the full cell was not enough.

`--no-fail-fast` is the fix for the stopping. This script is the fix for the
*silence*: it compares the test binaries cargo **built** against the ones the
run log shows it **started**, and fails if any was never reached. A truncated
cell can then no longer read as a complete one.

    cargo test --workspace --all-features --no-run --message-format=json \
        > test-targets.json
    cargo test --workspace --all-features --no-fail-fast 2>&1 | tee test-run.log
    scripts/ci-test-cell-report.py --targets test-targets.json --log test-run.log

The comparison is between two independent facts — what was compiled, and what
was started — rather than between the log and itself. That matters: a
fail-fast truncation leaves the log internally consistent (every `Running` line
it contains does have a `test result:` after it), so a log-only check cannot see
it. Only the compiled-target list knows what is missing.

Writes a table to `$GITHUB_STEP_SUMMARY` when that is set, and to stdout always.

Exit codes:

    0  every compiled test binary was started
    1  at least one was not, or the inputs are unusable

Note what this does NOT claim. It says which *binaries* ran, not which tests
inside them: `--no-fail-fast` makes cargo run every binary, and a binary that
aborts part-way through still reports its own totals. Doc-tests are listed
because they are part of the run, but they are not in the compiled-artifact
list, so their absence is reported rather than gated.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

# ANSI colour, which this log DOES carry: `ci.yml` sets `CARGO_TERM_COLOR:
# always` at the workflow level, so cargo emits `\e[1m\e[92m     Running\e[0m …`
# and libtest emits `test result: \e[32mok\e[0m. …`. Stripped before every match.
#
# This is not a hypothetical. The first CI run of this script reported `0 of 101
# compiled test binaries were started` against a cell that had just run all 101,
# because it was developed against a terminal-less local log and the regexes
# anchored on `^\s*Running`. It failed loudly, which is the right way round —
# but a reporter that cannot read the log is a reporter that cannot report.
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
# `     Running unittests src/lib.rs (target/debug/deps/roteiro-1a2b3c)` and
# `     Running tests/serve_mode_selection_cli.rs (target/debug/deps/serve-4d5e)`.
RUNNING = re.compile(r"^\s*Running\s+(?P<what>.+?)\s+\((?P<path>[^)]+)\)\s*$")
# `   Doc-tests roteiro`
DOCTESTS = re.compile(r"^\s*Doc-tests\s+(?P<crate>\S+)\s*$")
# `test result: FAILED. 12 passed; 1 failed; 3 ignored; 0 measured; 0 filtered out`
RESULT = re.compile(
    r"^test result:\s+(?P<verdict>ok|FAILED)\.\s+"
    r"(?P<passed>\d+) passed;\s+(?P<failed>\d+) failed;\s+(?P<ignored>\d+) ignored"
)


@dataclass
class Target:
    """One test binary cargo compiled, and what the log says happened to it."""

    package: str
    name: str
    kind: str
    executable: str  # basename, which carries cargo's hash and so is unique
    started: bool = False
    verdict: str | None = None
    counts: dict[str, int] = field(default_factory=dict)

    @property
    def label(self) -> str:
        return f"{self.package} · {self.kind} `{self.name}`"


def parse_targets(path: Path) -> list[Target]:
    """The test binaries in a `cargo test --no-run --message-format=json` stream.

    Only `profile.test` artefacts with an `executable` are test binaries: the
    same stream carries build-script and dependency artefacts, which are
    compiled and never run by anybody.
    """
    out: list[Target] = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue  # `json-render-diagnostics` still puts rendered text here
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        executable = msg.get("executable")
        if not executable or not msg.get("profile", {}).get("test"):
            continue
        target = msg.get("target", {})
        kinds = target.get("kind") or ["?"]
        out.append(
            Target(
                package=package_name(msg.get("package_id", "?")),
                name=target.get("name", "?"),
                kind=kinds[0],
                executable=os.path.basename(executable),
            )
        )
    return out


def package_name(package_id: str) -> str:
    """The crate name out of a `package_id`.

    Cargo has used two spellings — `roteiro 1.2.3 (path+file:///…)` and the
    newer `path+file:///…#roteiro@1.2.3` — and a reader that knows only one
    reports every package as `?` on the other. Both appear in the wild across
    the toolchains this repository builds with.
    """
    if "#" in package_id:
        tail = package_id.rsplit("#", 1)[1]
        name = tail.split("@", 1)[0]
        # `path+file:///…/crates/rto-graph#1.2.3` — no name before the version.
        if name and not name[0].isdigit():
            return name
        return package_id.rsplit("#", 1)[0].rstrip("/").rsplit("/", 1)[-1]
    return package_id.split(" ", 1)[0]


def parse_log(
    path: Path, by_executable: dict[str, Target]
) -> tuple[list[tuple[str, str | None]], bool]:
    """Mark every target the log shows started; return the doc-test runs, and
    whether the log contained any recognisable test output at all.

    Cargo runs test binaries one at a time, so a `test result:` line belongs to
    the most recent `Running`/`Doc-tests` heading. Attribution is by that
    ordering rather than by parsing the harness's own output, which carries no
    binary name at all.

    The second return value separates two things that otherwise look identical
    in the report: a cell that truncated (some binaries never started) and a
    reader that could not parse the log (none of them did, while the log is full
    of test output). Those want opposite responses from whoever reads the job,
    and the first CI run of this script needed exactly that distinction.
    """
    saw_test_output = False
    doctests: list[tuple[str, str | None]] = []
    current: Target | None = None
    current_doctest: int | None = None
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = ANSI.sub("", raw).rstrip()
        running = RUNNING.match(line)
        if running:
            current_doctest = None
            current = by_executable.get(os.path.basename(running.group("path")))
            if current is not None:
                current.started = True
            continue
        doc = DOCTESTS.match(line)
        if doc:
            current = None
            doctests.append((doc.group("crate"), None))
            current_doctest = len(doctests) - 1
            continue
        result = RESULT.match(line)
        if result:
            saw_test_output = True
            if current is not None:
                current.verdict = result.group("verdict")
                current.counts = {
                    k: int(result.group(k)) for k in ("passed", "failed", "ignored")
                }
            elif current_doctest is not None:
                crate, _ = doctests[current_doctest]
                doctests[current_doctest] = (crate, result.group("verdict"))
    return doctests, saw_test_output


def render(
    targets: list[Target],
    doctests: list[tuple[str, str | None]],
    saw_test_output: bool,
) -> tuple[str, bool]:
    """The report, and whether the cell is complete."""
    missing = [t for t in targets if not t.started]
    lines: list[str] = []
    lines.append("## Test targets this cell actually ran")
    lines.append("")
    if not targets:
        lines.append(
            "**No compiled test binaries were found.** That is not a clean run — "
            "it means this report could not look, which is the shape of failure "
            "issue #906 is about. Check that the `--no-run` step produced "
            "`--message-format=json` output."
        )
        return "\n".join(lines), False

    ran = len(targets) - len(missing)
    lines.append(f"**{ran} of {len(targets)} compiled test binaries were started.**")
    lines.append("")
    if ran == 0 and saw_test_output:
        # Not a truncation. The log plainly contains test results, so the tests
        # ran and this script failed to read the log — which is a defect in the
        # reporter, not in the cell, and saying "nothing ran" about it would send
        # the reader looking for a failure that is not there.
        lines.append(
            "**This is a reading failure, not a truncated cell.** The log "
            "contains `test result:` lines, so the binaries did run — this "
            "script could not match them to the compiled list. Check the "
            "`Running` line format first: `ci.yml` sets `CARGO_TERM_COLOR: "
            "always`, so those lines carry ANSI escapes, and that is exactly "
            "what broke this script's first CI run. Do not read the table "
            "below as evidence about the tests."
        )
        lines.append("")
    elif missing:
        lines.append(
            "The cell is **truncated**: the binaries below were built and never "
            "run, so nothing in them was verified by this run. Without "
            "`--no-fail-fast` that happens whenever an earlier binary fails, and "
            "the job reports a failure that looks like it covered everything."
        )
        lines.append("")
        for target in missing:
            lines.append(f"- ❌ **NOT RUN** — {target.label}")
        lines.append("")

    lines.append("| Package | Target | Ran | Result | passed | failed | ignored |")
    lines.append("| --- | --- | --- | --- | ---: | ---: | ---: |")
    for target in sorted(targets, key=lambda t: (t.package, t.kind, t.name)):
        verdict = target.verdict or ("—" if target.started else "never started")
        mark = "✅" if target.started else "❌"
        counts = target.counts
        lines.append(
            f"| {target.package} | {target.kind} `{target.name}` | {mark} | "
            f"{verdict} | {counts.get('passed', '')} | {counts.get('failed', '')} | "
            f"{counts.get('ignored', '')} |"
        )

    lines.append("")
    if doctests:
        summary = ", ".join(
            f"{crate} ({verdict or 'no result line'})" for crate, verdict in doctests
        )
        lines.append(f"Doc-tests run: {summary}.")
    else:
        lines.append("Doc-tests run: none.")
    lines.append("")
    lines.append(
        "Doc-tests are reported, not gated: they are not compiled artefacts, so "
        "this script has no independent list of which ones *should* have run."
    )
    return "\n".join(lines), not missing


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--targets",
        required=True,
        type=Path,
        help="`cargo test --no-run --message-format=json` output",
    )
    parser.add_argument("--log", required=True, type=Path, help="the teed `cargo test` output")
    args = parser.parse_args()

    for path in (args.targets, args.log):
        if not path.is_file():
            report = (
                f"## Test targets this cell actually ran\n\n"
                f"`{path}` is missing, so this cell cannot say what it ran. The "
                f"step that produces it did not get that far — read the failure "
                f"above rather than this report."
            )
            emit(report)
            return 1

    targets = parse_targets(args.targets)
    doctests, saw_test_output = parse_log(args.log, {t.executable: t for t in targets})
    report, complete = render(targets, doctests, saw_test_output)
    emit(report)
    return 0 if complete else 1


def emit(report: str) -> None:
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(report)
            handle.write("\n")


if __name__ == "__main__":
    sys.exit(main())

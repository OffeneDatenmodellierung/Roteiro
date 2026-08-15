# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.**

Report it privately through GitHub: go to the
[Security tab](https://github.com/OffeneDatenmodellierung/Roteiro/security) and
choose **Report a vulnerability**. That opens a private advisory visible only to
you and the maintainers, and it is the preferred route because the fix, the
disclosure and the credit all happen in one place.

If that option is not available to you, open a public issue that says only that
you have found a security problem and are looking for a private channel — no
details, no reproduction — and a maintainer will arrange one.

Useful things to include, to the extent you have them: what you did, what
happened, which version or commit, and which feature flags were enabled. Roteiro
keeps most of its surface behind opt-in features (`serve`, `models`,
`inference-local-models`, `pdf-text`, `image-ocr`, `image-vision`,
`audio-transcribe`, `execution`), so knowing whether a build was affected usually
depends on knowing which of those were on.

## What you can expect

Roteiro is a small project maintained in people's own time. It will not promise a
response deadline it cannot keep, so instead, plainly:

- Reports are read. You will get a human reply acknowledging the report, and if
  the first one is slow, a nudge on the same thread is welcome rather than rude.
- You will be told what we concluded — including if we conclude it is not a
  vulnerability, and why. A report that turns out to be a non-issue still gets an
  answer.
- If it is a real vulnerability, we will agree the disclosure timing with you
  rather than announcing it from under you.
- You will be credited in the advisory unless you would rather not be.

There is no bug bounty.

## Which versions get fixes

The latest published release. Roteiro releases from `main` and does not maintain
release branches, so a fix ships as a new version rather than as a backport.

## Scope

In scope: the crates in this repository (`roteiro`, `rto-graph`, `rto-spec`,
`rto-render`, `rto-serve`, `rto-llama`, `rto-exec`), the CLI, the MCP server, the
HTTP server and the explorer web app.

Out of scope, in the sense that we are not the right people to fix it: a
vulnerability in a **vendored upstream** such as llama.cpp or SQLite should go to
that project, which will fix it far faster than we can. Please tell us as well, so
we can pull the fix in — [`docs/VENDORED_DEPENDENCIES.md`](docs/VENDORED_DEPENDENCIES.md)
lists what is vendored, at which version, and where each project takes reports.

## How dependency risk is handled

Deliberately, and written down: see
[ADR-0017](docs/adr/0017-dependency-security-policy.md). In short — dependencies
are kept current automatically, no release is adopted until it is at least 48
hours old, `cargo deny` and `cargo audit` run in CI across the whole feature
matrix, and vendored native code is tracked by name because `cargo audit` cannot
see it.

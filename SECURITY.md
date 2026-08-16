# Security policy

## Reporting a vulnerability

**Please do not put the details in a public issue.**

**Preferred, if it is available to you:** open GitHub's
[Security tab](https://github.com/OffeneDatenmodellierung/Roteiro/security) and
look for **Report a vulnerability**. That creates a private advisory visible only
to you and the maintainers, and it is the best route because the report, the fix,
the disclosure and the credit all live in one place.

**If you do not see that button, it is not switched on yet — and that is on us,
not on you.** GitHub's private reporting is a per-repository setting, and it may
still be off. Do not let that stop you: open a public issue containing **only**
that you have found a security problem and would like a private channel. No
details, no version, no reproduction — a maintainer will arrange somewhere private
and take it from there. An almost-empty issue is not a disclosure; it is a
knock on the door.

Either way, do not post the details publicly first.

Useful things to include, to the extent you have them: what you did, what
happened, which version or commit, and which feature flags were enabled. Roteiro
keeps its heavier surface behind opt-in features (`serve`,
`inference-local-models`, `pdf-text`, `image-ocr`, `image-vision`,
`audio-transcribe`, `exec-boxlite`), so knowing whether a build was affected
usually depends on knowing which of those were on.

`execution`, `models` and `exec-subprocess` are **on by default** — assume all
three unless the reporter says otherwise. Concretely, a stock
`cargo install roteiro` has the consent-gated downloader (`ureq`/`rustls`)
compiled in, and can execute an analyzer as a child process on the host if the
operator passes `--allow-unsandboxed`. Reports touching either path should say
so; `--no-default-features --features execution` is the build that can do
neither.

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
are kept current automatically, no release is adopted until it has been published
for at least 48 hours (3 days as configured), `cargo deny` runs in CI across the
whole feature matrix alongside `cargo audit`, and vendored native code is tracked
by name because `cargo audit` cannot see it.

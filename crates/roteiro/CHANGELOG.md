# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [5.4.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v5.3.0...roteiro-v5.4.0) - 2026-09-02

### Added

- *(okf)* restore `okf validate` and `okf lint`, over our own checks

### Fixed

- *(okf)* three review findings, and the fifth false compile claim

## [5.3.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v5.2.0...roteiro-v5.3.0) - 2026-09-02

### Added

- *(okf)* add `roteiro okf syntax`, and give rto-okf-syntax a consumer

### Fixed

- *(okf-syntax)* a real line, a real bug, and two false rejections

### Other

- Merge pull request #733 from OffeneDatenmodellierung/feat/okf-syntax

## [5.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v5.1.0...roteiro-v5.2.0) - 2026-09-02

### Added

- *(okf)* make the validator a default dependency, not a feature
- *(cli)* add `roteiro okf` — trust, links, diff, validate, lint
- *(okf)* discover a peer's bundle, ask once, and screen what it says

### Fixed

- *(okf)* drop okf-validator, and keep okf-core
- *(okf)* skip symlinks in the fixture walk, and state `lint`'s exit status
- *(okf)* attribute a confirmation to the verifier that supports it
- *(okf)* order findings by severity, and never manufacture an actor token
- *(okf)* `--json` selected an output format and changed behaviour
- *(okf)* a peer's broken bundle must not fail our scan or delete their concepts
- *(okf)* ordinary markup was an evasion, and two silences told the wrong story
- *(okf)* a recorded grant is standing, `<div hidden>` conceals, and a bundle must close its frontmatter

### Other

- Merge pull request #715 from OffeneDatenmodellierung/fix/signal-provenance-break
- *(cli)* say that `okf links --broken` filters the human output only
- *(okf)* bring the discovery module's own argument up to date

## [5.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v5.0.0...roteiro-v5.1.0) - 2026-09-01

### Added

- *(okf)* read a peer's OKF bundle as external knowledge
- *(serve)* handle SIGHUP on every server, and reload the whole server

### Fixed

- *(okf)* harden the bundle walk and pin the reader's output order

### Other

- *(okf)* pin that a filled placeholder survives a rebuild

## [5.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v4.2.0...roteiro-v5.0.0) - 2026-09-01

### Added

- *(mcp)* [**breaking**] spell `debt`'s category filter `categories` on both surfaces

### Fixed

- *(tools)* refuse a tool argument key neither surface recognises

### Other

- *(tools)* say what a non-object `arguments` means here, and add no debt

## [4.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v4.1.0...roteiro-v4.2.0) - 2026-09-01

### Added

- *(review)* a whole-change verdict, and a warning when the reviewer wrote it

### Fixed

- *(review)* bound the verdict prompt's head, and correct what normalisation claims
- *(review)* name what a truncated prompt cut, and resolve the `--llm` base
- *(review)* say what `--base` compared against, and warn when it is stale

### Other

- Merge pull request #697 from OffeneDatenmodellierung/feat/649-verdict-and-provenance

## [4.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v4.0.0...roteiro-v4.1.0) - 2026-09-01

### Added

- *(serve)* each tool is stated to the model exactly once

### Fixed

- *(serve)* the prompt stops claiming a graph it was not given
- *(tests)* a signed tag is an annotated tag, and wants a message
- *(spec)* a blueprint is rendered, and the comment said otherwise
- *(spec)* two headings claiming one id, and one of them lost
- *(ci)* the matrix never once turned `execution` off

### Other

- Merge pull request #682 from OffeneDatenmodellierung/fix/667-ci-feature-matrix

## [4.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v3.3.0...roteiro-v4.0.0) - 2026-08-29

### Added

- *(okf)* [**breaking**] delete the Obsidian vault renderer
- *(okf)* nest a workspace bundle by member
- *(okf)* render the bundle, and cap a slug the filesystem would refuse
- *(mcp)* a session should not pay for tools it will never call

### Fixed

- *(init)* the failure message split a path across a line continuation
- *(init)* [**breaking**] the hook this PR installs invoked a command this PR deleted
- *(okf)* a cross-repo link landed on the stub standing in for its target
- *(okf)* a title could write its own `verified` block
- *(okf)* a shallow clone would have re-created the attribution it just fixed
- *(okf)* a link resolved by guesswork, and a review nobody did
- *(serve)* a refusal must not invent a remedy that cannot work
- *(mcp)* a tool this build never had was not withheld from anyone
- *(test)* prove the safecrlf fixture reproduces, and correct a stale doc
- *(diff)* a warning on stderr must not discard the diff
- *(check)* staging a file no longer hides its drift

### Other

- *(okf)* the shallow-boundary skip is not the only thing holding that guard
- *(serve)* say at the risk site that this predicate is startup-only
- *(serve)* resolve the tool selection once, not once per predicate call
- *(check)* clean up the scratch directory like every other test here
- *(diff)* a failed fixture setup must not present as an unsupported platform

## [3.3.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v3.2.1...roteiro-v3.3.0) - 2026-08-28

### Added

- *(review)* show the change, not only what the graph knows about it
- *(ci)* gate every local link in the rendered site

### Fixed

- *(diff)* trim the newline, not every trailing space
- *(diff)* git answers for itself — no external differ, no colour
- *(review)* a file HEAD still has is not an addition
- *(test)* decode percent-escapes as bytes, and share `repo_root`
- *(test)* a unique scratch dir, and say why the render failed
- *(ci)* read the newest check run, and stop the guards skipping in silence
- *(ci)* let the publish gate read the checks it waits for

### Other

- *(diff)* `git diff HEAD` is the worktree, not what a commit records
- *(diff)* the three ranges compare three different pairs of trees
- *(review)* an empty diff is not a mode change
- run the formatter over the new guard test
- *(release)* publishing waits for CI on the commit it publishes

## [3.2.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v3.2.0...roteiro-v3.2.1) - 2026-08-27

### Fixed

- *(spec)* a link written inside a heading belongs to that heading
- *(render)* a member with no root fails the manifest rather than vanishing

### Other

- *(render)* one definition per tool description, not two
- *(serve)* one authority for a shared tool description, and a measure of it
- *(spec)* pin the one heading construct the two sides still disagree on
- Merge branch 'main' into fix/621-parse-dont-scan
- Merge pull request #641 from OffeneDatenmodellierung/fix/post-merge-review-follow-ups

## [3.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v3.1.0...roteiro-v3.2.0) - 2026-08-26

### Added

- *(render)* the manifest says what each member deploys, and against which hub

### Fixed

- *(render)* the caption agrees with its count, and the doc with its code

### Other

- *(api)* the served topology reads the lifted rule

## [3.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v3.0.0...roteiro-v3.1.0) - 2026-08-26

### Added

- *(graph)* a config file that names an image is declaring a pin

### Other

- Merge pull request #627 from OffeneDatenmodellierung/dependabot/cargo/yaml-rust2-0.12.0
- Merge pull request #630 from OffeneDatenmodellierung/feat/431-guard-every-public-enum
- *(guard)* a scan that cannot read a file must fail, not skip it
- *(guard)* every public enum in the workspace must decide, not just rto-remote's

## [3.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.3.0...roteiro-v3.0.0) - 2026-08-22

### Added

- *(render)* the workspace vault carries findings, settings and a manifest
- *(api)* the topology view is a directed graph, not a star

### Fixed

- *(test)* read the `id` attribute, and drop the import the rewrite orphaned
- *(spec)* make the walk marker-driven, and stop the doc claiming more than the code
- *(spec)* [**breaking**] a heading's id is one rule, honoured by the graph and the renderer
- *(render)* quote every analyzer field, and stop the manifest overpromising
- *(api)* keep the hub in the topology when it has no outgoing refs

### Other

- Merge pull request #622 from OffeneDatenmodellierung/feat/438-justify-your-allow
- *(api)* record why a link target is safe under the projects filter

## [2.3.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.2.0...roteiro-v2.3.0) - 2026-08-22

### Added

- *(links)* persist authored [[links]], making the `authored → gold` path reachable
- *(links)* document the pinned matrix, and say when no comparison was possible
- *(links)* let --matrix resolve each spoke against its own hub pin

### Fixed

- *(render)* count declared links before the cap, and unbreak default-features clippy
- *(links)* refuse the hub flags too, count pruned edges, and test the vault
- *(links)* the no-op envelope keeps the contract, and an unknown value is no baseline
- *(links)* scope the pinned baseline to its file, and stop two shapes drifting
- *(links)* measure each pinned cell against the revision its spoke deploys

### Other

- Merge pull request #613 from OffeneDatenmodellierung/fix/610-one-definition-per-response-shape

## [2.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.1.1...roteiro-v2.2.0) - 2026-08-22

### Fixed

- *(cli)* make Refreshed #[must_use], so forgetting it is a build failure
- *(cli)* do not claim a tree the read never refreshed to
- *(cli)* address review — name the flag, not the token, and stop pinning prose
- *(cli)* read the working tree, and stop a read rewriting the store

### Other

- *(cli)* fold the nine announcement copies into one method

## [2.1.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.1.0...roteiro-v2.1.1) - 2026-08-22

### Fixed

- *(init)* put SKILL.md frontmatter first, and assert its position

## [2.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.0.2...roteiro-v2.1.0) - 2026-08-22

### Added

- *(mcp)* let the operator restrict the advertised tool surface (--tools)

### Fixed

- *(workspace,config)* say where a scan stopped, and which file is read

### Other

- Merge pull request #594 from OffeneDatenmodellierung/feat/mcp-surface-bundle
- *(site)* document `[mcp] tools`, the scan depth, and what --tools bounds

## [2.0.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.0.1...roteiro-v2.0.2) - 2026-08-21

### Fixed

- *(serve,spec)* strip `<think>` on `/v1`, and refuse an unterminated block

### Other

- Merge pull request #589 from OffeneDatenmodellierung/fix/thinking-582-583

## [2.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v2.0.0...roteiro-v2.0.1) - 2026-08-21

### Fixed

- *(render)* name a key the vault holds, and fail the guard on what it cannot read

### Other

- *(render)* describe the note-name rule, not the pre-#574 spelling

## [2.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.30.0...roteiro-v2.0.0) - 2026-08-21

### Fixed

- *(render)* [**breaking**] make note_name injective under filename case folding

### Other

- Merge remote-tracking branch 'origin/main' into fix/574-lossless-note-names

## [1.30.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.29.0...roteiro-v1.30.0) - 2026-08-21

### Added

- *(render)* render an Obsidian vault for a whole workspace (#442 part 1)

### Other

- Merge remote-tracking branch 'origin/main' into feat/442-workspace-vault
- *(render)* the qualified key and the note name are two strings

## [1.29.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.28.0...roteiro-v1.29.0) - 2026-08-20

### Fixed

- *(lint)* unbreak `main` under the clippy that ships with Rust 1.98

## [1.27.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.27.1...roteiro-v1.27.2) - 2026-08-20

### Fixed

- *(explorer)* one palette under one set of names, values per view

## [1.27.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.27.0...roteiro-v1.27.1) - 2026-08-20

### Fixed

- *(spec)* an ADR note in the vault carries the decision it names
- *(render)* a prose note in the vault is the document, not 6% of it

### Other

- *(spec,render)* pin the split — a section note is its own section
- *(render)* pin that a source file node is not given its bytes

## [1.27.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.26.6...roteiro-v1.27.0) - 2026-08-20

### Added

- *(sandbox)* user-supplied analyzer images, digest-pinned

### Other

- Merge remote-tracking branch 'origin/main' into feat/user-sandbox-images

## [1.26.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.26.5...roteiro-v1.26.6) - 2026-08-20

### Fixed

- *(links)* do not count a global --hub-rev as spokes that pinned
- *(cli)* three refusals that did not say the thing the reader needed

### Other

- Merge pull request #536 from OffeneDatenmodellierung/test/441-landing-page-bar-names
- Merge pull request #535 from OffeneDatenmodellierung/fix/522-453-505-small-refusals
- *(cli)* drop the apostropheless possessives from two test names
- Merge remote-tracking branch 'origin/main' into fix/522-453-505-small-refusals

## [1.26.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.26.3...roteiro-v1.26.4) - 2026-08-19

### Fixed

- *(render)* resolve every local link the docs site serves (#456, #457, #508)

## [1.26.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.26.0...roteiro-v1.26.1) - 2026-08-19

### Fixed

- show every workspace form, and count each repo once

## [1.26.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.25.0...roteiro-v1.26.0) - 2026-08-19

### Added

- *(llama)* size the context window per request, per model

### Fixed

- *(config)* render `max_context_tokens`'s `0` as its meaning, not as `0`
- *(build)* compile `--no-default-features --features execution` clean

### Other

- Merge pull request #497 from OffeneDatenmodellierung/feat/openai-client-tools
- *(lint)* gate `lint_cli.rs` on the backend it drives

## [1.25.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.24.0...roteiro-v1.25.0) - 2026-08-19

### Added

- *(mcp)* expose the two read-only `security` subcommands, scoped and bounded

### Fixed

- *(security)* the cross-reference guard the model-facing document claimed to have
- *(security)* `ready` must mean ready, not "its assets are provisioned"

### Other

- Merge pull request #468 from OffeneDatenmodellierung/feat/mcp-security-list-status

## [1.24.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.23.0...roteiro-v1.24.0) - 2026-08-19

### Added

- *(lint)* the sandboxed builder, so sandbox-by-default has something to select
- *(lint)* sandboxed by default; host execution is opt-in and layered
- *(lint)* report clippy at a point in time, and store none of it

### Fixed

- *(lint)* a refusal has two axes to be blind to, not one
- *(lint)* build refusals from lines, and test the default feature set
- *(lint)* refuse a relative scratch root; snapshot contents, not names
- *(lint)* set the scratch target dir rather than inheriting it

### Other

- Merge pull request #455 from OffeneDatenmodellierung/feat/sandboxed-builder
- Merge pull request #444 from OffeneDatenmodellierung/feat/website-rendered-pages

## [1.23.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.22.0...roteiro-v1.23.0) - 2026-08-18

### Fixed

- *(review)* apply `[debt] ignore` by taking the marker set from `debt`

### Other

- *(review)* drop the possessive from the debt-ignore test name

## [1.22.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.21.1...roteiro-v1.22.0) - 2026-08-18

### Added

- *(security)* run analyzers sandboxed by default, and never fall back

### Other

- Merge pull request #407 from OffeneDatenmodellierung/feat/security-run-sandboxed

## [1.21.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.21.0...roteiro-v1.21.1) - 2026-08-18

### Fixed

- *(query)* search reads `limit = 0` as unlimited, per channel, via `window`

### Other

- *(cli,mcp)* say what `limit` means on every surface that describes it

## [1.21.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.20.0...roteiro-v1.21.0) - 2026-08-18

### Added

- *(review)* the reviewer, and the vacuous zero its instrument walked into

### Fixed

- *(review)* one reviewable-diff rule, and unreviewable paths are reported
- *(review)* a mod.rs is gated by its directory's declaration, not `mod mod;`

### Other

- truncation reported on both surfaces; caps raised
- reviewer truncation detection (rto-graph half)
- *(review)* UNREVIEWED checkpoint - Stage 35b PR 1 in progress

## [1.20.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.19.0...roteiro-v1.20.0) - 2026-08-17

### Fixed

- *(remote)* refuse a `[remote] model` that squats on a local id; 4xx a refused gate
- *(graph)* say which class each retained cache object was kept for
- *(graph)* reclaim superseded object-cache generations

### Other

- Merge origin/main into feat/stage34b-surface-wiring
- Merge pull request #392 from OffeneDatenmodellierung/fix/limit-zero-means-unlimited
- Merge pull request #394 from OffeneDatenmodellierung/fix/object-cache-reclaim

## [1.19.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.18.0...roteiro-v1.19.0) - 2026-08-17

### Added

- *(remote)* the transport, and the promises it makes false

### Other

- Merge branch 'main' into feat/stage34b-remote-transport
- Merge main: keep the newest half of each side in the plan

## [1.18.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.17.0...roteiro-v1.18.0) - 2026-08-17

### Added

- *(remote)* `[remote]` config with inverted precedence, and `roteiro remote`

### Fixed

- *(debt)* a placeholder is a thing you build, not a stub you owe

### Other

- Merge pull request #381 from OffeneDatenmodellierung/feat/stage34-remote-model-tier
- *(remote)* drop the unused `rto-exec` edge from the `remote` feature
- *(remote)* the consent gate, end to end through real files and flags

## [1.17.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.16.0...roteiro-v1.17.0) - 2026-08-17

### Added

- *(models)* one resolver decides which model serves a task, and says why

### Fixed

- *(config)* a `[models]` value that names no model reads as unset everywhere
- *(init)* put "Proving a negative" in the skill template, not just its output

### Other

- *(init)* diff the skill artifacts with split('\n'), not lines()

## [1.16.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.15.0...roteiro-v1.16.0) - 2026-08-17

### Added

- *(cli)* `roteiro debt-density`, and the five other surfaces

### Fixed

- *(render)* `_Home` scopes intent debt by `[debt] ignore`, both tables

## [1.15.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.14.0...roteiro-v1.15.0) - 2026-08-17

### Fixed

- *(exec)* format, and correct the feature-gating claim the message relies on

### Other

- *(exec)* record the runtime-verification trade, and fix the stale selection rule

## [1.14.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.13.0...roteiro-v1.14.0) - 2026-08-17

### Added

- *(roteiro)* make `models` a default feature

### Other

- *(features)* UNREVIEWED checkpoint - exec-subprocess default + prefetch/status move

## [1.13.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.12.0...roteiro-v1.13.0) - 2026-08-16

### Fixed

- *(sync)* refuse to rewrite a graph store written by a newer build

### Other

- Merge pull request #348 from OffeneDatenmodellierung/fix/store-newer-than-binary
- rustfmt and clippy on the store-guard test
- *(cli)* cover the gate chokepoint the write guard also protects
- *(store)* cover the store-from-the-future write guard

## [1.12.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.11.0...roteiro-v1.12.0) - 2026-08-16

### Added

- *(cli)* prefetch the OSV databases, and render the cross-reference
- *(rto-exec)* osv-scanner adapter and a download-by-URL asset source

### Fixed

- *(config)* stop reporting a `[debt] ignore_reset` that never happened
- *(security)* refuse an asset body whose completeness cannot be established

### Other

- Merge origin/main (migrations 13 + osv-scanner) into fix/guardrails-319-321-324-330
- Merge pull request #340 from OffeneDatenmodellierung/feat/stage25-memory-recall
- *(cli)* end-to-end security ingest and cross-reference
- cover the two guarded behaviours that had no test

## [1.11.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.10.1...roteiro-v1.11.0) - 2026-08-16

### Added

- *(model)* add `roteiro model rm`, and show installed size in `model list`
- *(models)* resume interrupted model downloads instead of restarting
- *(memory)* settle scope — the anchor is the scope test (ADR-0013 v1.1)
- *(cli)* roteiro memory add|list|forget
- *(explorer)* surface generated media content, always attributed
- *(media)* add the pre-generation gate, recorded not silent
- *(media)* move generated content to its own artifact store

### Fixed

- *(media)* stop `media status` calling a gate refusal a generated record
- *(media)* correct the generation counter and the status advice

### Other

- Merge branch 'main' into feat/model-lifecycle
- Merge pull request #317 from OffeneDatenmodellierung/feat/stage23-agent-memory
- *(media)* add the deferred `media` argument-shape tests

## [1.10.1](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.10.0...roteiro-v1.10.1) - 2026-08-15

### Fixed

- *(llama)* share one backend per process

## [1.10.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.9.0...roteiro-v1.10.0) - 2026-08-15

### Added

- *(exec)* add AnalyzerRunner contract and security ingest

### Fixed

- *(extract)* drop cached vision/ASR engines before exit

## [1.9.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.8.0...roteiro-v1.9.0) - 2026-08-14

### Added

- *(ask)* ground answers with search snippets + stricter grounding prompt
- *(serve)* scope the workspace Ask to the selected workspace

### Fixed

- *(serve)* prove per-workspace routing + correct app.js header comment

## [1.8.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.7.0...roteiro-v1.8.0) - 2026-08-14

### Added

- *(explorer)* move workspace Ask panel under the drill-into row
- *(explorer)* model dropdown + openable cited nodes in Ask panels

### Fixed

- *(explorer)* preserve the Ask model pick across idempotent re-renders

## [1.7.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.6.0...roteiro-v1.7.0) - 2026-08-14

### Added

- *(explorer)* add a workspace-level Ask panel across all projects

### Fixed

- *(explorer)* linkify project-qualified keys in workspace Ask answers

### Other

- Merge pull request #279 from OffeneDatenmodellierung/fix/chat-embedding-model-guard
- Merge pull request #278 from OffeneDatenmodellierung/fix/mcp-cli-test-isolation

## [1.6.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.5.0...roteiro-v1.6.0) - 2026-08-14

### Added

- *(telemetry)* route native llama.cpp + ggml logs through tracing
- *(telemetry)* opt-in rotating OTEL-JSON file logging (ADR-0011)

### Fixed

- *(telemetry)* address PR #274 Copilot review

### Other

- Merge remote-tracking branch 'origin/main' into feat/file-logging-rotation

## [1.5.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.4.0...roteiro-v1.5.0) - 2026-08-14

### Added

- *(cli)* make `serve` the network HTTP server; add `roteiro mcp`

### Fixed

- *(cli)* address PR #268 review comments

### Added

- *(mcp)* `roteiro mcp` — the MCP graph server as a first-class command: STDIO by
  default, `--http ADDR` for networked MCP, carrying the `--workspace`/`-w`/
  `--sync-on-access` options.

### Changed

- *(serve)* **`roteiro serve` is now the network HTTP server** (previously the
  stdio MCP server). Bare `roteiro serve` binds `[serve] addr` (default
  `127.0.0.1:8017`) and serves the OpenAI-compatible `/v1` endpoint (+ graph tools
  + Ask) when built `--features serve` with a model installed, always alongside the
  read-only `/v1/graph/*` API and the `/` web UI; a build without the model feature
  (or with no model installed) degrades to the llama-free graph API + UI instead of
  failing. **Migration:** point MCP-client configs at `roteiro mcp` instead of
  `roteiro serve`. The old spellings still work as **deprecated aliases**, each
  printing a one-line stderr notice: `serve --models` (now the default — flag is
  redundant) and `serve --http ADDR` (→ `mcp --http ADDR`). No flags were removed;
  nothing hard-breaks.

## [1.4.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.3.0...roteiro-v1.4.0) - 2026-08-14

### Added

- *(config)* derive config keys from typed Rust config structs
- *(explorer)* workspace-selector landing + route-by-type + sticker logo

### Fixed

- *(config)* address PR #266 review — explicit value-absence + doc fixes
- *(cli)* exit cleanly on closed stdout pipe (SIGPIPE), no broken-pipe panic

### Other

- Merge pull request #266 from OffeneDatenmodellierung/feat/config-keys-from-rust-structs
- Merge remote-tracking branch 'origin/main' into feat/app-config-only-filter
- Merge pull request #262 from OffeneDatenmodellierung/feat/links-canonical-key-match
- Merge pull request #261 from OffeneDatenmodellierung/feat/serve-all-workspaces
- Merge pull request #260 from OffeneDatenmodellierung/feat/explorer-infer-links-live
- Merge pull request #259 from OffeneDatenmodellierung/fix/cli-sigpipe-broken-pipe
- *(cli)* gate broken-pipe test to linux+macos for hardcoded SIGPIPE=13
- Merge pull request #255 from OffeneDatenmodellierung/feat/explorer-selector-landing

## [1.3.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.2.0...roteiro-v1.3.0) - 2026-08-13

### Added

- *(explorer)* graph-grounded Ask tab (llama-gated) [PR 8]

### Fixed

- *(explorer)* address PR #251 Copilot review — Ask needs a served model, trim key punctuation, keyboard-activate ref links

## [1.2.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.1.0...roteiro-v1.2.0) - 2026-08-13

### Added

- *(explorer)* follow-the-link hop — config_key→struct bridge + cross-repo jump [PR 7]
- *(explorer)* spoke cross-repo link rendering (config→app-key + drift) [PR 6]
- *(explorer)* project graph view + panels + matrix provenance [PR 5]
- *(explorer)* workspace-view UI (topology + override matrix + drift) [PR 4]
- *(explorer)* llama-free graph server + /v1/graph/workspaces (multi-workspace)
- *(config)* multi-workspace + standalone config and WorkspaceSet (links selector)
- *(explorer)* read-only /v1/graph JSON API (PR 1/5 — data foundation)
- *(models)* readable model list + Qwen3-Coder-30B-A3B registry entry

### Fixed

- *(explorer)* address PR #249 Copilot review — no-alloc kind check, narrowed struct lookup, non-null workspace, drift wording
- *(explorer)* index cross-repo links per-edge, not per-target [PR 6 review]
- *(explorer)* address PR #245 Copilot review (edge id, ARIA tabs, hash decode)
- *(explorer)* drop unhosted topology edges; cache static assets (PR #243 review)
- *(explorer)* validate --workspace-name at startup; dedup graph.db path
- *(explorer)* update serve-path graph_api call site to the new signature
- *(config)* address PR #239 Copilot review (linked-name collisions, standalone invariant, deferred set build)
- *(explorer)* address PR #237 review comments

### Other

- Potential fix for pull request finding

## [1.1.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v1.0.0...roteiro-v1.1.0) - 2026-08-13

### Added

- *(links)* [pins] ref templates + published-artifact fetch (ADR-0009 step 8c)
- *(links)* --pinned auto-resolves each spoke against the version it pins (ADR-0009 step 8b)
- version-pin resolution — resolve cross-repo drift at the deployed hub version (ADR-0009 step 8a)
- *(graph)* git submodule pins (ADR-0009 derived deploy-artifact extraction, part 2)
- *(links)* cross-repo config override matrix + drift view (ADR-0009 step 7)
- *(links)* persist inferred cross-repo edges, read config keys from graph (ADR-0009 2b)
- *(graph)* config-key nodes are graph-native (ADR-0009)
- *(serve)* follow cross-repo links in the served tools (ADR-0009)
- *(links)* infer cross-repo config links by key matching (ADR-0009)
- *(links)* cross-repo authored links + `roteiro links` (ADR-0009)

### Fixed

- *(links)* address PR #224 review — surface config errors, tolerate bad artifacts
- *(links)* address PR #222 review — match hub dir name, cheaper pin resolution
- *(graph)* address PR #221 review — resolve any revspec, test path fixes
- *(graph)* address PR #218 review — index-aware submodule pins
- *(links)* address PR #215 review — flag constraints, per-file value lookup
- *(links)* clear stale inferred edges + CI clippy, address PR #213 review

### Other

- *(deps)* upgrade dependencies to latest MSRV-1.94-compatible
- *(deps)* bump toml 0.9 → 1.1
- *(serve)* address PR #210 review
- *(links)* address PR #207 review
- *(links)* address PR #206 review

## [1.0.0](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.19...roteiro-v1.0.0) - 2026-08-11

First stable release — the public API is now covered by SemVer; breaking changes will bump the major version.

### Added
- **Multi-repo workspace serve (ADR-0008).** `roteiro serve --workspace <root>` (and
  a `[workspace]` config table) hosts every git repo under a root from one process,
  holding the model once and selecting a project per request. `--models --mcp` serves
  the OpenAI `/v1` endpoint and the `/mcp` graph tools **on one port**; `--sync-on-access`
  (re)builds a project's graph on first touch. `SIGHUP` reloads the registry live.

### Changed
- `serve` flags are validated in clap: `--mcp` requires `--models`, `--http` conflicts
  with `--models`.


## [0.0.19](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.18...roteiro-v0.0.19) - 2026-08-11

### Added

- *(init)* ship an installable agent skill; enrich the AGENTS.md block
- *(cli)* add `roteiro search` — text search from the plain CLI
- *(query)* search captured content + rank curated/overview above test symbols

### Other

- wire the new `roteiro search` CLI into the skill/AGENTS/website
- address PR #182 review
- *(test)* correct search_cli doc comment — code symbol, not test symbol

## [0.0.18](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.17...roteiro-v0.0.18) - 2026-08-11

### Other

- Merge pull request #176 from OffeneDatenmodellierung/chore/debt-precision
- *(debt)* precision pass — intent-debt now reflects real debt (97 → 6)

## [0.0.17](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.16...roteiro-v0.0.17) - 2026-08-11

### Added

- *(review)* distinguish "added" files from "modified"

### Fixed

- *(review)* address PR #169 review — open-set doc, comment, added test

## [0.0.16](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.15...roteiro-v0.0.16) - 2026-08-11

### Added

- *(audio)* transcribe audio into the graph via llama.cpp mtmd ([#18](https://github.com/OffeneDatenmodellierung/Roteiro/pull/18))
- *(serve)* in-app TLS for the model endpoint (ADR-0002 follow-up)

### Fixed

- *(audio)* address PR #161 review — reject dual media; fix stale Ultravox docs

### Other

- Merge pull request #161 from OffeneDatenmodellierung/feat/audio-ingestion
- address PR #155 review on TLS wiring

## [0.0.14](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.13...roteiro-v0.0.14) - 2026-08-10

### Added

- *(lat)* import @lat: source backlinks as authored edges
- *(review)* range mode — review a commit range with --base (§5d)
- *(ci)* publish the graph artifact on merge + tree-verified load (Stage 14)
- *(review)* CLI-first graph-grounded review of the current change (Stage 17)
- *(check)* worktree-aware check + pre-commit drift gate (Stage 16)
- *(spec)* draft generation via llama.cpp + ADR-0003 amendment (Stage 20)
- *(models)* coding/reasoning generative models + role label (Stage 20)

### Fixed

- address PR #131 review — range-specific empty-review message
- address PR #123 second review — verify tree-less artifacts, republish always, no races
- address PR #112 review — ADR-edit drift, deterministic order, related-kind
- address PR #99 review — v1.2 refs, dedup vlm doc, reject non-embedding model
- *(spec)* address PR #94 review — strip <think>, correct backend docs
- *(models)* address PR #93 review — tokenizer.json + deterministic default

### Other

- Merge branch 'main' into feat/lat-backlink-import
- Merge branch 'main' into feat/sync-index
- Merge pull request #131 from OffeneDatenmodellierung/feat/range-review
- Merge branch 'main' into feat/ci-artifact
- clarify check validates the working tree, not the git index (PR #109 review)
- Merge branch 'main' into feat/model-residency
- remove duplicated helpers (Stage 14 health check)
- remove candle — unify the whole inference core on llama.cpp (ADR-0003 v1.2)
- Merge branch 'main' into feat/stage20-models

## [0.0.13](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.12...roteiro-v0.0.13) - 2026-08-09

### Added

- *(serve)* vision serving — multimodal /v1/chat/completions via llama.cpp mtmd (ADR-0006)
- *(serve)* /v1/embeddings from GGUF via llama.cpp (ADR-0006, completes Stage 19)
- *(serve)* auto-register graph tools — code-aware serving (ADR-0006, Stage 19b)
- *(serve)* wire `roteiro serve --models` + [serve] config (ADR-0006, Stage 19a)
- *(config)* wire [ingest] content toggles into sync (ADR-0007)
- *(config)* layered roteiro.toml with CLI>project>user>default precedence (Stage 18)
- *(generator)* Qwen3 support + curated generative tiers (low/mid/high)
- *(models)* opinionated model matrix — resource tiers per section
- *(extract)* Tier A image OCR via ocrs/rten (Stage 12, ADR-0005)
- *(context)* dependency-aware per-node context cache (Stage 12)
- *(infer)* semantic + structural duplicate detection (Stage 12)
- *(extract)* ingest PDF text into meta.content (Stage 12, feature-gated)
- *(spec)* warn on debug builds in `spec draft` (candle is slow unoptimized)
- *(spec)* `roteiro spec draft` — Tier 1 offline local-model drafting (completes Stage 13)
- *(spec)* blueprint scaffold kind — `spec scaffold --kind blueprint`
- *(spec)* `roteiro spec scaffold` — grounded, check-clean ADR skeletons (Stage 13, Tier 0)
- *(spec)* `roteiro spec context` — graph-grounded authoring context (Stage 13, Tier 0)

### Fixed

- *(serve)* address PR #86 review — tool loop robustness
- *(config)* address PR #79 review — repo-root discovery, provenance help, missing-feature warn
- *(spec)* PR #56 review — validate `--kind` before the graph build

### Other

- Merge branch 'main' into feat/streaming-downloads
- *(models)* stream model downloads to disk instead of buffering in memory
- *(models)* decouple the model registry/pull from the candle feature

## [0.0.12](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.11...roteiro-v0.0.12) - 2026-08-09

### Added

- *(oracle)* codegraph validation oracle — `import --from codegraph` (Stage 11, 3/3)
- *(import)* lat.md importer — authored layer over the code graph (Stage 11, 2/3)
- *(import)* durable import layers surviving code-changing syncs (Stage 11)
- *(debt)* intent-debt tracking — marker nodes + `roteiro debt` (Stage 15)
- *(rto-spec)* Graphify importer — `roteiro import --from graphify` (Stage 9)
- inference-local-models tier — candle embedder + model registry (Stage 8)

### Fixed

- *(oracle)* PR #46 review — propagate DB errors, count-based scope diff, both samples
- *(import)* PR #44 review round 2 — symlink-safe walk, docs, sort
- *(import)* stamp lat edges with LAT_REF; reject lat files outside the repo
- *(import)* validate import layers on import and on sync; PR review
- *(render)* resolve [[wiki-links]] and publish the Build Plan page
- *(clippy)* backtick BUILD_PLAN path in import doc comment
- address PR #33 review — model validation, checksum warning, tests

### Other

- make Graphify import durability explicit (PR #35 review)

## [0.0.11](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.10...roteiro-v0.0.11) - 2026-08-08

### Added

- *(rto-graph)* offline inference layer — `roteiro infer` (Stage 8 core)

### Fixed

- *(infer)* address PR #31 review — authoritative re-infer, stem, perf, docs

## [0.0.10](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.9...roteiro-v0.0.10) - 2026-08-08

### Added

- *(rto-graph)* portable graph artifacts — `export`/`load` (Stage 10 part 1)

## [0.0.9](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.8...roteiro-v0.0.9) - 2026-08-08

### Added

- *(query)* `roteiro path` + MCP `path` tool (Stage 5/7 follow-up)

### Fixed

- address PR #25 review — path invariant, stderr, tool description

## [0.0.8](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.7...roteiro-v0.0.8) - 2026-08-08

### Added

- *(mcp)* adopt rmcp for stdio + networked HTTP serving (ADR-0002)
- *(rto-render)* MCP server over stdio, feature-gated (Stage 7)

## [0.0.7](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.6...roteiro-v0.0.7) - 2026-08-08

### Added

- *(rto-render)* real docs-site + Obsidian renderers (Stage 6)

## [0.0.6](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.5...roteiro-v0.0.6) - 2026-08-08

### Added

- *(roteiro)* `init` + git hooks + AGENTS.md (Stage 5, part 2)
- query surface with stable --json schema (Stage 5, part 1)
- *(rto-spec)* authored ADR layer and `roteiro check` (Stage 4)

### Fixed

- *(rto-spec)* address PR #14 review — fail on malformed ADRs, fix stale doc

### Other

- Merge pull request #15 from OffeneDatenmodellierung/release-plz-2026-08-08T09-17-25Z
- Merge remote-tracking branch 'origin/main' into feat/authored-layer

## [0.0.5](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.4...roteiro-v0.0.5) - 2026-08-08

### Added

- *(rto-graph)* uncommitted working-tree dirty overlay (Stage 3)

## [0.0.4](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.3...roteiro-v0.0.4) - 2026-08-08

### Added

- *(rto-graph)* derived tree-sitter Rust extraction (Stage 3)

## [0.0.3](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.2...roteiro-v0.0.3) - 2026-08-08

### Added

- *(rto-graph)* content-addressed cache and `roteiro sync` (Stage 2)

## [0.0.2](https://github.com/OffeneDatenmodellierung/Roteiro/compare/roteiro-v0.0.1...roteiro-v0.0.2) - 2026-08-07

### Added

- *(rto-graph)* implement graph core (Stage 1)

## [0.0.1](https://github.com/OffeneDatenmodellierung/Roteiro/releases/tag/roteiro-v0.0.1) - 2026-08-07

### Added

- Initial commit

# Roteiro

**A provenance-tagged knowledge graph for your codebase.**

The Portuguese *roteiros* were the guarded pilot books of the Age of Discovery —
accumulated route knowledge that made navigation repeatable. Roteiro does the
same for a codebase: structure, intent, and context in one queryable store,
for humans and AI agents alike.

Every edge in the graph records **how it was produced**:

| Provenance | Source | Nature |
|---|---|---|
| `derived` | tree-sitter extraction (full symbol graph for Rust today; more languages rolling out via tree-sitter `tags`) | Deterministic — symbols, calls, imports |
| `authored` | ADRs, blueprints, `// @rto:` annotations | Curated intent, drift-checked in CI |
| `inferred` | Docs, PDFs, images, embeddings | Fuzzy suggestions with confidence scores |

One SQLite store. One query surface. Three renderers: a docs website, an
OKF bundle, and an optional MCP server (`--features mcp`) — all build
outputs of the same graph, so what humans review is what agents query. *Build
output* is meant literally: `render` **deletes and rebuilds its output directory
every time**, so keep your own notes outside the bundle and link into it — see
[docs/OKF_BUNDLE.md](docs/OKF_BUNDLE.md), which also covers how concept paths
are derived and what replaced the Obsidian vault in 4.0.0. Offline
by default — one optional, default-off feature can call a hosted model, and
[it is described below](#one-capability-sends-your-repositorys-content-elsewhere-it-is-off);
git-native and content-addressed, so the graph is shareable across a team.

## Getting started

```sh
cargo install roteiro          # lean default build; offline unless you ask it to fetch
cd your-repo
roteiro init                   # store + git hooks + AGENTS.md
roteiro sync                   # build the graph
roteiro review                 # your change, plus what the graph knows about it
```

The default build needs no toolchain class beyond the C compiler Rust itself
requires — no C++, no cmake, no libclang — and makes no network call on its own.
It includes everything needed to *prepare* for working offline —
`roteiro model pull` and `roteiro security prefetch|status|run` — so
[`docs/OFFLINE_SETUP.md`](docs/OFFLINE_SETUP.md) needs no special build. Nothing
is fetched until you say yes to it. `security run` runs the analyzer **on this
host** and requires `--allow-unsandboxed` on every invocation, because that
executes a third-party analyzer here with no isolation boundary; without the flag
it refuses. Roteiro has no sandbox — the opt-in microVM backend was removed
([ADR-0030](docs/adr/0030-remove-the-microvm-sandbox.md)). **The analyzers themselves are
yours to install** (`semgrep`, `osv-scanner`, `cargo install cargo-audit`) — the
guide has the commands, and `roteiro security ingest` accepts a report produced
anywhere if you would rather install none of them. Local *inference* and
*serving* remain opt-in (`--features inference-local-models`, `--features
serve`).

`roteiro lint clippy` is the other shape, and it differs twice over
([ADR-0020](docs/adr/0020-build-capable-sandboxed-execution.md)). It **stores
nothing** — no findings layer, no history, no `lint list` — because an advisory
id is *assigned* and assignment is a promise, while a lint name is a symbol in a
compiler, renamed or removed at its discretion: the first is a durable fact about
the repository, the second an opinion about the code as it stands today. And it
**refuses until you grant host execution**: linting compiles the tree, so it runs
that tree's build scripts and proc macros here, and that the toolchain is yours
does not make the code yours when you are reviewing somebody else's branch. You
opt in explicitly — `--allow-unsandboxed` for one run, or `[lint]
allow_unsandboxed = true` in **your own** `~/.roteiro/config.toml` as a standing
preference. A project's committed `roteiro.toml` may deny it for everyone and may
never grant it, and the flag does not override that denial
([`docs/SANDBOXED_LINTING.md`](docs/SANDBOXED_LINTING.md)).

### Models run where you point them — on this machine by default

Everything above runs on your machine, and so does the default model setup: a
local OpenAI-compatible server such as Ollama, named by `[provider] base_url`
([docs/MODEL_SETUP.md](docs/MODEL_SETUP.md)). Roteiro makes no network call of
its own with that configuration.

`base_url` may instead name a **hosted** endpoint — an enterprise Anthropic or
OpenAI subscription, or a company gateway
([ADR-0028](docs/adr/0028-one-engine-local-or-hosted.md)). Then the model gets
the same full context the local one does: the conversation, graph tool results,
symbol names, paths and captured prose. Function bodies are not in the graph,
but what is sent still identifies a codebase.

- **Every run says so.** Whenever the provider is not on this machine, Roteiro
  prints its host on stderr before the first request.
- **Any config layer may set it**, including a committed `roteiro.toml`, so a
  team can standardise on one endpoint. Read a project's `[provider]` table
  before running Roteiro in a repository you did not configure.
- **Plaintext is refused off this machine**: a hosted `base_url` must be
  `https://`. The credential is `ROTEIRO_PROVIDER_API_KEY`, never a config key.
- **Roteiro keeps no log of what it sent.** A hosted provider's own audit log is
  the record.

So *"nothing leaves the machine"* is true of Roteiro **as configured by
default**. [ADR-0006](docs/adr/0006-local-model-serving.md) states the same
sentence scoped to `roteiro serve`, which still never downloads.

Planning to work on a train or a plane? Models and analyzer databases must be
fetched once, deliberately, before you disconnect —
[`docs/OFFLINE_SETUP.md`](docs/OFFLINE_SETUP.md) is that one-time preparation,
including the air-gapped route.

See <https://roteiro.dev> for the full guide (modes, local models, languages,
config), and [ADR-0001](docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md)
plus [`docs/history/BUILD_PLAN.md`](docs/history/BUILD_PLAN.md) for the design and roadmap.
Contribution + review standards live in [`AGENTS.md`](AGENTS.md).

### Upgrading from a build with the sandbox

The `exec-boxlite` feature is gone
([ADR-0030](docs/adr/0030-remove-the-microvm-sandbox.md)): drop
`--features exec-boxlite` from install commands, `--sandboxed`/`--image` from
scripts, and `[security]`/`[lint] image` from config files (they load, with a
warning that they are inert). Delete `~/.roteiro/security/boxlite-home` and
`~/.roteiro/security/boxlite-runtime` by hand if present — the first can hold
many GB of images.

## Logging

By default Roteiro logs human-readable text to **stdout**, unchanged. You can
*additionally* stream logs to a **rotating file** in an OpenTelemetry-shaped JSON
format (groundwork for a future OTLP collector — see
[ADR-0011](docs/adr/0011-structured-file-logging-otel-groundwork.md)):

```sh
roteiro --log <cmd>                                  # file at $ROTEIRO_HOME/logs/roteiro.log
roteiro --log-file /var/log/roteiro.log <cmd>        # explicit path (enables it)
roteiro --log-rotation hourly --log-format otel <cmd>
```

Or set it once in `roteiro.toml` (or `~/.roteiro/config.toml`):

```toml
[telemetry]
file = "~/.roteiro/logs/roteiro.log"   # unset ⇒ file logging OFF (stdout only)
rotation = "daily"                     # daily | hourly | minutely | never
format = "otel"                        # otel | json (alias) | text
```

Each flag also has an env var (`ROTEIRO_LOG_FILE`, `ROTEIRO_LOG_ROTATION`,
`ROTEIRO_LOG_FORMAT`); `ROTEIRO_LOG` sets level filter directives (e.g. `debug`).
Precedence is flag > env > project > user > default. Writes are non-blocking, so
logging to disk never stalls a command. The OTLP network exporter and metrics are
deferred; see the ADR for the field mapping and the seam.

## Workspace

- `crates/rto-graph` — SQLite store, provenance model, content-addressed cache, extraction, sync, query, inference
- `crates/rto-spec` — house-style ADR/blueprint parsing, `check` (drift gate), importers, spec authoring
- `crates/rto-faithful` — rendering faithfulness: every claim in a rendered summary must trace to a tool finding
- `crates/rto-render` — docs site, OKF bundle, MCP server (feature-gated)
- `crates/rto-serve` — local OpenAI-compatible model server (llama.cpp; feature-gated)
- `crates/rto-llama` — llama.cpp inference core (generation, embeddings, vision), shared by serving and internal uses
- `crates/roteiro` — umbrella CLI

## Community

Questions, ideas, or want to see what others are building with it? Join the
[Roteiro Discord](https://discord.gg/bxgj4w6KM).

Bugs and feature requests are better as
[issues](https://github.com/OffeneDatenmodellierung/Roteiro/issues) — they stay
searchable and get linked from the graph.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Contributions are accepted under the same terms.

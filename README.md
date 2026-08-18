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
Obsidian vault, and an optional MCP server (`--features mcp`) — all build
outputs of the same graph, so what humans review is what agents query. Offline
by default — one optional, default-off feature can call a hosted model, and
[it is described below](#one-capability-sends-your-repositorys-content-elsewhere-it-is-off);
git-native and content-addressed, so the graph is shareable across a team.

## Getting started

```sh
cargo install roteiro          # lean default build; offline unless you ask it to fetch
cd your-repo
roteiro init                   # store + git hooks + AGENTS.md
roteiro sync                   # build the graph
roteiro review                 # graph-grounded review of your change
```

The default build needs no toolchain class beyond the C compiler Rust itself
requires — no C++, no cmake, no libclang — and makes no network call on its own.
It includes everything needed to *prepare* for working offline —
`roteiro model pull` and `roteiro security prefetch|status|run` — so
[`docs/OFFLINE_SETUP.md`](docs/OFFLINE_SETUP.md) needs no special build. Nothing
is fetched until you say yes to it. `security run` is sandboxed by default and
refuses by name when this build has no sandbox; running the analyzer **on this
host** instead still requires `--allow-unsandboxed` on every invocation, because
that executes a third-party analyzer here with no isolation boundary. **The analyzers themselves are
yours to install** (`semgrep`, `osv-scanner`, `cargo install cargo-audit`) — the
guide has the commands, and `roteiro security ingest` accepts a report produced
anywhere if you would rather install none of them. Local *inference* and
*serving* remain opt-in (`--features inference-local-models`, `--features
serve`).

`roteiro lint clippy` is the other shape, and the difference is deliberate: it
runs the linter on this host, prints what it said, and **stores nothing** — no
findings layer, no history, no `lint list`. An advisory id is *assigned*, and
assignment is a promise; a lint name is a symbol in a compiler, renamed or
removed at its discretion. The first is a durable fact about the repository, the
second an opinion about the code as it stands today
([ADR-0020](docs/adr/0020-build-capable-sandboxed-execution.md)). Because the
linter compiles the tree, its build scripts and proc macros run here too — the
report names the toolchain, the feature set and the isolation it had, since there
is no stored run record to carry them.

### One capability sends your repository's content elsewhere. It is off.

Everything above runs on your machine. **One optional feature does not**, and it
is named here rather than left to be discovered: `--features remote` compiles the
**remote model tier** ([ADR-0019](docs/adr/0019-remote-model-tier.md)), which can
send graph-derived context to a hosted model.

- **It is not in the default build.** `cargo install roteiro` cannot send
  anything, and no release will change that. It *is* included in
  `--all-features`, so a build made that way can — with consent.
- **Enabling the feature does not enable the tier.** A run must be granted by
  your own `~/.roteiro/config.toml` **and** by the invocation
  (`--allow-remote`, or answering a prompt that shows you the exact bytes).
  Neither alone suffices. A committed `roteiro.toml` may **deny** it for a whole
  repository but can never grant it — a merged line must not authorise egress on
  a teammate's machine.
- **Three commands can send, and each has to be told to.** `roteiro remote call`
  is the one that exists to; `roteiro spec draft --allow-remote` drafts with the
  hosted model instead of a local one; `roteiro serve --allow-remote` makes it
  the model the Ask panel uses. Without the flag all three are local, and **only
  `remote call` ever prompts** — on the other two the flag is the only way,
  because a prompt on a command whose default is local turns a habituated "yes"
  into consent you never quite gave.
- **`serve --allow-remote` grants for the life of the server process**, not one
  request at a time, and that is a materially larger exposure than a one-shot
  command: every Ask that server answers sends context to the hosted model for as
  long as it runs, including requests from anyone else who can reach the port.
  `serve` binds loopback by default, your user config still has to have granted,
  and every call is on the ledger. The grant dies with the process and is never
  persisted.
- **A refused `--allow-remote` stops the run.** If you asked for the hosted model
  and a layer said no, Roteiro names the layer and does *not* answer from a local
  model instead — that would be a different answer with nothing to signal the
  change.
- **What would be sent is inspectable first, and what did is recorded.**
  `roteiro remote dry-run` prints the exact body and sends nothing;
  `roteiro remote log` reads the append-only ledger of what left, and when.
- **Source code is not sent** — function bodies are not in the graph. That is
  *not* the same as nothing identifying being sent: symbol names, file paths and
  captured prose go, they identify a codebase, and there is no redaction
  chokepoint on a prompt. `roteiro remote status` says so in full.
- **It never falls back quietly.** With the tier on and the endpoint
  unreachable, Roteiro fails with an error naming the endpoint rather than
  answering from a local model — a different model is a different answer.

So *"nothing leaves the machine"* is true of Roteiro **as shipped and as
configured**, and is no longer a property of the software as a whole.
[ADR-0006](docs/adr/0006-local-model-serving.md) states the same sentence scoped
to `roteiro serve`, which still exposes only installed models and still never
downloads.

Planning to work on a train or a plane? Models and analyzer databases must be
fetched once, deliberately, before you disconnect —
[`docs/OFFLINE_SETUP.md`](docs/OFFLINE_SETUP.md) is that one-time preparation,
including the air-gapped route.

See <https://roteiro.dev> for the full guide (modes, local models, languages,
config), and [ADR-0001](docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md)
plus [`docs/BUILD_PLAN.md`](docs/BUILD_PLAN.md) for the design and roadmap.
Contribution + review standards live in [`AGENTS.md`](AGENTS.md).

### Sandboxed analyzers (`--features exec-boxlite`)

Optional, off by default, and the only feature with build requirements the
default install does not have. It runs `semgrep` inside a digest-pinned OCI image
in a microVM — read-only worktree, no egress, no ambient credentials
([ADR-0014](docs/adr/0014-sandboxed-analyzer-execution.md)).

**1. `protoc >= 3.12` must be on the build host.** `boxlite-shared`'s build
script requires it unconditionally, and without it the failure is a build-script
error rather than a missing-feature message:

```sh
sudo apt-get install -y protobuf-compiler   # Debian/Ubuntu
brew install protobuf                       # macOS
```

**2. The sandbox runtime is verified at build time, whichever way you build.**
`boxlite` embeds a prebuilt runtime archive into the compiled library, which its
own build script fetches with an unverified `curl`. Roteiro pins the SHA-256 of
every file that archive contributes and checks them in `rto-exec`'s build script
— against what `boxlite` actually extracted, before anything links. A mismatch,
a missing file or an unpinned extra file fails the build.

```sh
# Just build it. The runtime is fetched by boxlite over TLS and every extracted
# file is verified against the pins; the build says so on its own output.
cargo install roteiro --features exec-boxlite

# Then pull the pinned analyzer image. Only this build can: the image half of
# `prefetch` is compiled out of any binary without `exec-boxlite`.
roteiro security prefetch --analyzer semgrep --allow-download
```

The second step is separate for a reason of ordering rather than taste: the
binary that can pull an image is the one the first step exists to produce.
Because a run never pulls, skipping it means `roteiro security run` refuses with
`ImageNotProvisioned` instead of fetching an image mid-scan.

**For a build that touches no network at all**, provision the archive first and
name it. `boxlite`'s `curl` then reads a local file and opens no socket, and the
bytes are verified *before* they are extracted as well as after:

```sh
cargo install roteiro                                           # default set has `prefetch`
roteiro security prefetch --analyzer sandbox --allow-download    # digest-pinned, ~26 MB
BOXLITE_RUNTIME_URL="file://$HOME/.roteiro/security/boxlite-runtime/boxlite-runtime.tar.gz" \
  cargo install roteiro --features exec-boxlite
```

Both paths are verified; only the second is offline. Do not describe the first
as air-gapped, and do not describe the second as the only safe one — the
difference is egress, not integrity
([ADR-0014 v1.3](docs/adr/0014-sandboxed-analyzer-execution.md)).

**3. It embeds third-party binaries, some of them GPL-2.0 and LGPL-2.0**, and
distributing a binary built this way carries source-offer duties. `prefetch`
prints the full notice before installing anything; it is also at
[`crates/rto-exec/NOTICE-boxlite-runtime.md`](crates/rto-exec/NOTICE-boxlite-runtime.md).
A default build embeds none of it.

Platforms: `darwin-arm64`, `linux-x64-gnu`, `linux-arm64-gnu`. Running a scan
also needs a usable hypervisor (`/dev/kvm` on Linux, `Hypervisor.framework` on
Apple Silicon); where there is none, the run says so by name and the sandbox
tests skip with a visible message.

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
- `crates/rto-render` — docs site, Obsidian vault, MCP server (feature-gated)
- `crates/rto-serve` — local OpenAI-compatible model server (llama.cpp; feature-gated)
- `crates/rto-llama` — llama.cpp inference core (generation, embeddings, vision), shared by serving and internal uses
- `crates/roteiro` — umbrella CLI

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Contributions are accepted under the same terms.

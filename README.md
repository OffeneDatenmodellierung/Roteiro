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
by default; git-native and content-addressed, so the graph is shareable across a
team.

## Getting started

```sh
cargo install roteiro          # lean, fully-offline default build
cd your-repo
roteiro init                   # store + git hooks + AGENTS.md
roteiro sync                   # build the graph
roteiro review                 # graph-grounded review of your change
```

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

**2. The sandbox runtime must be provisioned and verified before you build.**
`boxlite` embeds a prebuilt runtime archive into the compiled library, which its
own build script would otherwise fetch with an unverified `curl`. Roteiro fetches
it through the digest-pinned asset machinery instead, then points `boxlite` at
the local copy so its fetch never reaches the network:

```sh
# A build that can already provision — the subprocess feature is enough:
cargo install roteiro --features exec-subprocess
roteiro security prefetch --analyzer sandbox --allow-download

# Then build the sandboxed backend against the verified archive:
BOXLITE_RUNTIME_URL="file://$HOME/.roteiro/security/boxlite-runtime/boxlite-runtime.tar.gz" \
  cargo install roteiro --features exec-boxlite
```

The build **fails with the exact recipe** if `BOXLITE_RUNTIME_URL` is unset,
names a remote URL, or points at bytes that do not match the pin. That is
deliberate: it is the only point at which anything verifies what gets embedded.

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
- `crates/rto-render` — docs site, Obsidian vault, MCP server (feature-gated)
- `crates/rto-serve` — local OpenAI-compatible model server (llama.cpp; feature-gated)
- `crates/rto-llama` — llama.cpp inference core (generation, embeddings, vision), shared by serving and internal uses
- `crates/roteiro` — umbrella CLI

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Contributions are accepted under the same terms.

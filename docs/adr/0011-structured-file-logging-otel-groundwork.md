---
Title: Structured file logging — OpenTelemetry-shaped JSON, rotated, groundwork for OTLP
Space: ARCH
Parent: ADRs

# ADR-specific metadata (unknown keys are ignored; used for indexing/search)
type: adr
adr-id: "0011"
status: Accepted                    # Draft | For Review | Accepted | Rejected | Superseded
architectural-significance: MEDIUM  # SOFT | LOW | MEDIUM | HIGH | VERY HIGH
domain: Developer Tooling
decision-makers: ["The Roteiro Project Team"]
superseded-by:
version: "1.0"
last-modified: 2026-08-14
confluence-url:
---

# ADR-0011: Structured file logging — OpenTelemetry-shaped JSON, rotated, groundwork for OTLP

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | MEDIUM |
| **Domain** | Developer Tooling |
| **Document version** | 1.0 |

## Reference

Introduces an **opt-in second logging sink**: alongside the human-readable text on
stdout (unchanged), Roteiro can also write logs to a **rotating file** in a
structured, **OpenTelemetry-shaped JSON** format that a future collector can
ingest. It is the deliberate *groundwork* step for observability — the network
**OTLP exporter and metrics/traces are explicitly deferred** to a later ADR; this
one only lands the file sink and the seam. Configured via a new `[telemetry]`
table, governed by the layered-config rules of
[[docs/adr/0007-configuration-file.md]] and the offline/deterministic principles
of [[docs/adr/0001-build-roteiro-unified-codebase-knowledge-graph.md]].

## Summary

- Keep **stdout logging exactly as it is** — human text, default, always on.
- Add an **optional file sink**, off unless enabled, written by
  [`tracing`](https://docs.rs/tracing) + `tracing-subscriber` +
  `tracing-appender`. The subscriber is built in **one place**:
  `crates/roteiro/src/telemetry.rs::init`.
- **Non-blocking writes**: the file appender runs through
  `tracing_appender::non_blocking`, whose `WorkerGuard` is held for the whole
  process lifetime (a slow disk can never stall the CLI).
- **OTEL-friendly format**: each event is one JSON object per line, its fields
  mapped onto the OpenTelemetry [log data
  model](https://opentelemetry.io/docs/specs/otel/logs/data-model/) (table below).
- **Deferred**: no OTLP/network exporter, no metrics, no real `trace_id`
  correlation yet — the JSON shape and the single init seam are chosen so those
  drop in later without touching call sites.

## Context

Today every diagnostic in Roteiro is a `println!`/`eprintln!` to the process's
standard streams. That is fine for interactive use but gives an operator running
`roteiro serve` (ADR-0006) or the MCP server (ADR-0002) nothing durable or
machine-parsable to collect. The eventual goal is full OpenTelemetry — logs,
metrics, and traces over OTLP — but that is a large, network-facing dependency
surface we do not want to adopt in one step.

Forces to reconcile:

1. **Don't regress the default.** Stdout must stay human-readable and on;
   file logging is strictly additive and opt-in.
2. **Offline & dependency-light (ADR-0001).** No network exporter, no heavy OTEL
   SDK yet — just the `tracing` stack, which is the Rust ecosystem standard.
3. **Never stall the app.** Disk I/O for logs must not block a command; hence a
   non-blocking appender with a lifetime-held flush guard.
4. **Forward-compatible shape.** The on-disk records should already speak the
   OpenTelemetry log data model, so a future collector needs a thin mapping, not a
   reshape.
5. **One config surface, layered.** Reuse ADR-0007 precedence (CLI/env > project >
   user > default) rather than inventing a new mechanism.

## Decision makers

- The Roteiro Project Team

## Recommended option

Introduce a `[telemetry]` config table and a `telemetry` module that owns the
single subscriber-build seam.

### Why `[telemetry]`, not `[log]`

The table is named **`telemetry`** because it is the home for the whole deferred
observability story — OTLP **logs *and* metrics/traces** — not merely "the log
file". Naming it `telemetry` now avoids a rename (or a confusing second table)
when the exporter and metrics land.

### Config surface

```toml
[telemetry]
# Path to the rotating log file. Unset ⇒ file logging is OFF (stdout only).
# A leading `~/` expands to home; a relative path resolves under $ROTEIRO_HOME.
file = "~/.roteiro/logs/roteiro.log"
# daily (default) | hourly | minutely | never
rotation = "daily"
# otel (default) | json (alias of otel) | text (the same format stdout uses)
format = "otel"
```

Every field is optional. Overrides, in precedence order (flag beats env beats
config):

| Flag | Env var | Config key | Effect |
|---|---|---|---|
| `--log-file <PATH>` | `ROTEIRO_LOG_FILE` | `[telemetry] file` | Enable + set path |
| `--log` | — | — | Enable at the default path `$ROTEIRO_HOME/logs/roteiro.log` |
| `--log-rotation <CADENCE>` | `ROTEIRO_LOG_ROTATION` | `[telemetry] rotation` | Rotation cadence |
| `--log-format <FORMAT>` | `ROTEIRO_LOG_FORMAT` | `[telemetry] format` | On-disk format |
| — | `ROTEIRO_LOG` | — | `EnvFilter` level directives for both layers (e.g. `debug`) |

An invalid `rotation`/`format` value is a **hard error** at startup (never a
silent fallback), matching ADR-0007's "malformed config fails fast".

### OTEL log field mapping (`otel`/`json` format)

One JSON object per line, mapped onto the OpenTelemetry log data model:

| JSON field | OTEL field | Source |
|---|---|---|
| `time_unix_nano` | `TimeUnixNano` | wall-clock at emit, integer ns since the Unix epoch (OTEL's native representation) |
| `observed_time_unix_nano` | `ObservedTimeUnixNano` | same instant (we emit as we observe) |
| `severity_number` | `SeverityNumber` | tracing level → OTEL `1`/`5`/`9`/`13`/`17` (TRACE/DEBUG/INFO/WARN/ERROR) |
| `severity_text` | `SeverityText` | tracing level name |
| `body` | `Body` | the event's `message` |
| `attributes` | `Attributes` | remaining event fields + `code.namespace`/`code.filepath`/`code.lineno` source location + `span.name`/`span.path` context |
| `resource` | `Resource` | `service.name` = `roteiro`, `service.version` = crate version |

Integer nanoseconds (rather than an RFC3339 string) is chosen deliberately: it is
OTLP's own wire representation and needs no date-formatting dependency.

### Rotation behaviour

Rotation is **time-based**, delegated to `tracing_appender::rolling`:
`daily`/`hourly`/`minutely` append a date suffix to the file name (e.g.
`roteiro.log.2026-08-14`); `never` writes a single, unrotated file at the exact
path. **Size-based rotation is intentionally out of scope** — `tracing-appender`
does not offer it — and is noted as a candidate for the OTLP step. Old-file
pruning/retention is likewise deferred.

### Native llama.cpp / ggml log routing

The dominant source of stdout/stderr noise today is **not** Rust code — it is the
**native C logging** from llama.cpp + ggml. Loading a model floods the terminal
with hundreds of `llama_model_loader:` / `create_tensor:` / `print_info:` /
`ggml_metal_*` lines emitted straight from the C library, bypassing the `tracing`
subscriber entirely.

We tame this by calling `llama_cpp_2::send_logs_to_tracing(LogOptions::default())`
**once**, at engine construction in `crates/rto-llama/src/llama.rs`
(`install_native_log_bridge`, feature-gated on `llama`). It installs both the
`llama_log_set` and `ggml_log_set` callbacks, so every native line becomes a
`tracing` event on the `llama.cpp` / `ggml` target at its mapped level (ggml
`DEBUG`/`INFO`/`WARN`/`ERROR` → tracing `DEBUG`/`INFO`/`WARN`/`ERROR`). It is then
gated by exactly the same subscriber as everything else:

- **plain `roteiro serve`** (stdout filter `warn`): the verbose model-loader
  `INFO` wall is **suppressed**; only genuine native warnings/errors surface;
- **file logging on** (`--log`, file filter `info`): the wall is **captured** in
  the rotating OTEL file at its proper level, off the terminal;
- **`ROTEIRO_LOG=debug`** (or `info`): the wall is **surfaced** on stdout too.

Ordering matters two ways, both handled: the subscriber is installed in
`roteiro`'s `main` before any engine is built, and the bridge is installed
**before** `LlamaBackend::init()` — the backend's device probe (e.g. ggml-metal's
`ggml_metal_device_init` block) logs *during* init, so a later install would let
that first batch escape to stderr. A hand-rolled `llama_log_set` callback was
rejected: it needs `unsafe`, which is `forbid`den workspace-wide, whereas
`send_logs_to_tracing` is a safe wrapper (and also handles llama.cpp's `CONT`
continuation-line buffering and per-submodule targets for us).

The bridge lives entirely behind the `llama` feature, so non-llama builds are
unaffected.

### The deferred-OTLP seam

`telemetry::init` is the *only* place layers are assembled, so the future exporter
is a **third layer** added there — no call site changes. The JSON already carries
OTEL field names and a `resource`, so a collector mapping is thin. Real
`trace_id`/`span_id` correlation arrives with the OpenTelemetry layer
(`tracing-opentelemetry`) at that time; until then span **context** is surfaced as
the `span.name`/`span.path` attributes so the shape is already collector-friendly.
Metrics (an OTEL `MeterProvider`) attach at the same seam.

## Consequences

**Positive**

- Operators get durable, structured, machine-ingestible logs without a network
  dependency or any change to interactive stdout output.
- The non-blocking guard means logging to disk can never stall a command.
- The OTLP exporter + metrics become an additive change at one seam.
- The native llama.cpp/ggml log wall — the biggest source of stdout noise — is
  routed through `tracing`, so it obeys the log-level filter (quiet by default,
  opt-in at `debug`) and lands in the OTEL file when file logging is on.

**Negative / costs**

- Three new (well-maintained, `cargo deny`-clean) dependencies: `tracing`,
  `tracing-subscriber`, `tracing-appender`.
- Roteiro's own Rust-side `println!`/`eprintln!` diagnostics are **not** yet
  routed through `tracing` (the native llama.cpp/ggml logs now are). So the file
  captures the startup breadcrumb + native engine logs, but not yet the CLI's own
  `eprintln!` warnings. Migrating those Rust call sites to `tracing` is follow-up
  work, deliberately out of this ADR's scope.
- No retention/size cap yet; operators choosing `never` own the file's growth.

## Status

Accepted — implemented in `crates/roteiro/src/telemetry.rs` with the `[telemetry]`
config table in `crates/roteiro/src/config.rs`, and the native llama.cpp/ggml
log bridge in `crates/roteiro/../rto-llama/src/llama.rs` (feature `llama`). The
OTLP exporter, metrics, and the Rust-side `println!`/`eprintln!`→`tracing`
migration are tracked as follow-up.

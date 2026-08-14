//! Structured logging / telemetry init (ADR-0011).
//!
//! Roteiro's default logging is unchanged: human-readable text on **stdout**.
//! This module adds an **opt-in second sink** — a rotating file, written in an
//! OpenTelemetry-shaped JSON format a future collector can ingest — without
//! touching the stdout layer. It is deliberately dependency-light: `tracing` +
//! `tracing-subscriber` + `tracing-appender`, no OTLP/network exporter yet (that
//! is the deferred step this module leaves a seam for; see ADR-0011).
//!
//! # The single init seam
//!
//! [`init`] is the one place the subscriber is built. It composes a
//! [`tracing_subscriber::Registry`] with:
//!
//! - a **stdout** layer — the existing human text format, always present, its
//!   default filter `warn` so ordinary runs print nothing new;
//! - an **optional file** layer — added only when file logging is enabled,
//!   filtered at `info` by default, formatted per [`Format`].
//!
//! Both layers honour the `ROTEIRO_LOG` env var for filter directives (e.g.
//! `ROTEIRO_LOG=debug`), the standard `tracing` `EnvFilter` syntax.
//!
//! # OpenTelemetry log field mapping (`otel` / `json` format)
//!
//! Each event is one JSON object per line. Fields map onto the OpenTelemetry
//! [log data model](https://opentelemetry.io/docs/specs/otel/logs/data-model/):
//!
//! | JSON field | OTEL field | Source |
//! |---|---|---|
//! | `time_unix_nano` | `TimeUnixNano` | wall-clock at emit, ns since the Unix epoch |
//! | `observed_time_unix_nano` | `ObservedTimeUnixNano` | same instant (we emit as we observe) |
//! | `severity_number` | `SeverityNumber` | tracing level → OTEL 1/5/9/13/17 |
//! | `severity_text` | `SeverityText` | tracing level name (`TRACE`…`ERROR`) |
//! | `body` | `Body` | the event's `message` |
//! | `attributes` | `Attributes` | remaining event fields + `code.*` source location + `span.*` context |
//! | `resource` | `Resource` | `service.name` / `service.version` (constant for now) |
//!
//! Real `trace_id` / `span_id` correlation joins when the OTLP exporter lands;
//! until then span **context** is surfaced as the `span.name` / `span.path`
//! attributes so the shape is already collector-friendly.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{self, FmtContext, FormattedFields};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer, Registry};

use crate::config::{self, TelemetryConfig};

/// The env var read by both layers for `EnvFilter` directives (level filtering).
/// The path/rotation/format env vars (`ROTEIRO_LOG_FILE` / `_ROTATION` / `_FORMAT`)
/// are read by clap directly on the global flags (see `main`).
const ENV_FILTER: &str = "ROTEIRO_LOG";

/// A boxed layer over the shared [`Registry`], so the stdout and (differently
/// typed) file layers can live in one `Vec`.
type BoxLayer = Box<dyn Layer<Registry> + Send + Sync + 'static>;

/// Command-line / env overrides for telemetry, gathered in `main` from the global
/// clap flags (each already flag-or-env, flag winning). They take precedence over
/// the config file, per ADR-0007.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// `--log-file` / `ROTEIRO_LOG_FILE`: explicit log path (enables file logging).
    pub file: Option<String>,
    /// `--log`: enable file logging at the default path when no path is given.
    pub enable_default: bool,
    /// `--log-rotation` / `ROTEIRO_LOG_ROTATION`.
    pub rotation: Option<String>,
    /// `--log-format` / `ROTEIRO_LOG_FORMAT`.
    pub format: Option<String>,
}

/// Rotation cadence for the rolling file appender (time-based; `tracing-appender`
/// does not offer size-based rotation — that is deferred to the OTLP step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rotation {
    /// A new file each day (default).
    Daily,
    /// A new file each hour.
    Hourly,
    /// A new file each minute (mainly for tests / high-volume debugging).
    Minutely,
    /// One file, never rotated.
    Never,
}

impl Rotation {
    /// Parse the config/flag string; unknown values are a hard error so a typo
    /// never silently disables rotation.
    fn parse(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "daily" => Ok(Self::Daily),
            "hourly" => Ok(Self::Hourly),
            "minutely" => Ok(Self::Minutely),
            "never" => Ok(Self::Never),
            other => anyhow::bail!(
                "invalid telemetry rotation {other:?}: expected daily|hourly|minutely|never"
            ),
        }
    }

    /// Map to the `tracing-appender` rotation policy.
    fn appender(self) -> tracing_appender::rolling::Rotation {
        use tracing_appender::rolling::Rotation as R;
        match self {
            Self::Daily => R::DAILY,
            Self::Hourly => R::HOURLY,
            Self::Minutely => R::MINUTELY,
            Self::Never => R::NEVER,
        }
    }
}

/// On-disk record format for the file layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// One OpenTelemetry-shaped JSON object per line (see the module docs).
    Otel,
    /// The same human-readable text format stdout uses.
    Text,
}

impl Format {
    /// Parse the config/flag string; `otel` and `json` are synonyms.
    fn parse(s: &str) -> anyhow::Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "otel" | "json" => Ok(Self::Otel),
            "text" => Ok(Self::Text),
            other => anyhow::bail!("invalid telemetry format {other:?}: expected otel|json|text"),
        }
    }
}

/// The resolved, effective file-logging settings (`None` path ⇒ disabled).
#[derive(Debug, Clone)]
struct Settings {
    /// Absolute (or cwd-relative) path of the log file; `None` ⇒ file logging off.
    path: Option<PathBuf>,
    /// Rotation cadence.
    rotation: Rotation,
    /// On-disk format.
    format: Format,
}

/// Held by `main` for the whole process lifetime. Dropping it flushes and joins
/// the non-blocking appender's worker thread; the `WorkerGuard` **must** outlive
/// all logging, or buffered lines are lost on exit (hence we return it rather than
/// dropping it inside `init`).
#[derive(Debug)]
#[must_use = "hold the telemetry guard for the process lifetime; dropping it stops file logging"]
pub struct Guard(#[allow(dead_code)] Option<WorkerGuard>);

/// Resolve the effective file-logging settings from overrides (CLI/env) over
/// config (project/user) over built-in defaults.
fn resolve(overrides: &Overrides, cfg: &TelemetryConfig) -> anyhow::Result<Settings> {
    // Path precedence: `--log-file`/env > `[telemetry] file` > (`--log` ⇒ default).
    let raw_path = overrides
        .file
        .clone()
        .or_else(|| cfg.file.clone())
        .map(|p| resolve_path(&p));
    let path = match raw_path {
        Some(p) => Some(p),
        None if overrides.enable_default => Some(
            config::default_log_path()
                .context("cannot resolve the default log path (no ROTEIRO_HOME or home dir)")?,
        ),
        None => None,
    };

    let rotation = overrides
        .rotation
        .clone()
        .or_else(|| cfg.rotation.clone())
        .map_or(Ok(Rotation::Daily), |s| Rotation::parse(&s))?;
    let format = overrides
        .format
        .clone()
        .or_else(|| cfg.format.clone())
        .map_or(Ok(Format::Otel), |s| Format::parse(&s))?;

    Ok(Settings {
        path,
        rotation,
        format,
    })
}

/// Expand a leading `~/` and anchor a relative path under `$ROTEIRO_HOME`
/// (else `~/.roteiro`), so a bare `roteiro.log` lands beside the config rather
/// than in an arbitrary cwd. An absolute path is used verbatim.
fn resolve_path(raw: &str) -> PathBuf {
    let expanded = config::expand_tilde(raw).into_owned();
    if expanded.is_absolute() {
        return expanded;
    }
    config::roteiro_home().map_or(expanded.clone(), |home| home.join(&expanded))
}

/// Build the subscriber and install it as the global default (ADR-0011). The
/// stdout layer is always present and unchanged; the file layer is added only when
/// file logging is enabled. Returns the [`Guard`] the caller must hold for the
/// process lifetime.
///
/// # Errors
/// A malformed rotation/format value, an unresolvable default path, a directory
/// that cannot be created, or a global subscriber already being installed.
pub fn init(overrides: &Overrides, cfg: &TelemetryConfig) -> anyhow::Result<Guard> {
    let settings = resolve(overrides, cfg)?;

    // Stdout: the existing human text format, kept default. Its filter defaults to
    // `warn` (env `ROTEIRO_LOG`) so ordinary runs print nothing new — stdout stays
    // exactly as it is today, since the app emits no tracing events yet.
    let stdout = fmt::layer().with_filter(env_filter(Level::WARN)).boxed();
    let mut layers: Vec<BoxLayer> = vec![stdout];

    let mut guard = None;
    if let Some(path) = &settings.path {
        let (layer, worker) = file_layer(path, settings.rotation, settings.format)?;
        layers.push(layer);
        guard = Some(worker);
    }

    Registry::default()
        .with(layers)
        .try_init()
        .context("installing the global tracing subscriber")?;

    if settings.path.is_some() {
        // A breadcrumb the file captures (info) but stdout suppresses (warn) — so
        // the file has content on every real run without perturbing stdout.
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            "roteiro telemetry initialised"
        );
    }
    Ok(Guard(guard))
}

/// An `EnvFilter` reading `ROTEIRO_LOG` for directives, falling back to `default`.
fn env_filter(default: Level) -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(default.into())
        .with_env_var(ENV_FILTER)
        .from_env_lossy()
}

/// Build the rotating, non-blocking file layer for `path`. Split out so the init
/// test can compose it onto a **scoped** subscriber without racing on the
/// process-global default `init` installs. Returns the layer and its non-blocking
/// worker guard.
///
/// # Errors
/// The parent directory cannot be created.
fn file_layer(
    path: &std::path::Path,
    rotation: Rotation,
    format: Format,
) -> anyhow::Result<(BoxLayer, WorkerGuard)> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .context("telemetry log path has no file name")?;
    let dir = dir.map_or_else(|| PathBuf::from("."), PathBuf::from);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating log directory {}", dir.display()))?;

    let appender = tracing_appender::rolling::RollingFileAppender::new(
        rotation.appender(),
        &dir,
        std::path::Path::new(file_name),
    );
    let (writer, worker) = tracing_appender::non_blocking(appender);

    // The file always filters at `info` by default (env `ROTEIRO_LOG` still wins),
    // independent of the quieter stdout default.
    let layer = match format {
        Format::Otel => fmt::layer()
            .event_format(OtelJson)
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(env_filter(Level::INFO))
            .boxed(),
        Format::Text => fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(env_filter(Level::INFO))
            .boxed(),
    };
    Ok((layer, worker))
}

/// The custom event formatter mapping a `tracing` event to an OpenTelemetry-shaped
/// JSON line (see the module docs for the field table).
struct OtelJson;

impl<S, N> FormatEvent<S, N> for OtelJson
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();

        // Event fields: `message` → OTEL `body`, everything else → attributes.
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);
        let mut attributes = visitor.attributes;

        // Source location → OTEL `code.*` attributes.
        attributes.insert("code.namespace".into(), meta.target().into());
        if let Some(file) = meta.file() {
            attributes.insert("code.filepath".into(), file.into());
        }
        if let Some(line) = meta.line() {
            attributes.insert("code.lineno".into(), line.into());
        }

        // Span context: the enclosing span names (root→leaf) plus each span's
        // recorded fields. Real trace/span ids arrive with the OTLP exporter.
        if let Some(scope) = ctx.event_scope() {
            let mut names = Vec::new();
            for span in scope.from_root() {
                names.push(span.name().to_owned());
                let ext = span.extensions();
                if let Some(fields) = ext.get::<FormattedFields<N>>()
                    && !fields.fields.is_empty()
                {
                    attributes.insert(
                        format!("span.{}.fields", span.name()),
                        fields.fields.as_str().into(),
                    );
                }
            }
            if let Some(leaf) = names.last() {
                attributes.insert("span.name".into(), leaf.clone().into());
            }
            attributes.insert("span.path".into(), names.join(" > ").into());
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let unix_nano = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
        let (severity_number, severity_text) = severity(*meta.level());

        let record = serde_json::json!({
            "time_unix_nano": unix_nano,
            "observed_time_unix_nano": unix_nano,
            "severity_number": severity_number,
            "severity_text": severity_text,
            "body": visitor.body.unwrap_or_default(),
            "attributes": attributes,
            "resource": {
                "service.name": "roteiro",
                "service.version": env!("CARGO_PKG_VERSION"),
            },
        });

        let line = serde_json::to_string(&record).map_err(|_| std::fmt::Error)?;
        writeln!(writer, "{line}")
    }
}

/// Map a `tracing` level to the OpenTelemetry `(SeverityNumber, SeverityText)`
/// pair. OTEL numbers each range in fives; we use the range base for each level.
fn severity(level: Level) -> (u8, &'static str) {
    match level {
        Level::TRACE => (1, "TRACE"),
        Level::DEBUG => (5, "DEBUG"),
        Level::INFO => (9, "INFO"),
        Level::WARN => (13, "WARN"),
        Level::ERROR => (17, "ERROR"),
    }
}

/// Collects an event's fields into an OTEL `body` (the `message` field) and an
/// `attributes` map (everything else), preserving value types where JSON allows.
#[derive(Default)]
struct JsonVisitor {
    /// The `message` field, mapped to OTEL `Body`.
    body: Option<String>,
    /// All other fields, mapped to OTEL `Attributes`.
    attributes: serde_json::Map<String, serde_json::Value>,
}

impl JsonVisitor {
    /// Route a field: the special `message` becomes the body, all else an attribute.
    fn put(&mut self, field: &tracing::field::Field, value: serde_json::Value) {
        if field.name() == "message" {
            self.body = value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| Some(value.to_string()));
        } else {
            self.attributes.insert(field.name().to_owned(), value);
        }
    }
}

impl tracing::field::Visit for JsonVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field, value.into());
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.put(field, value.into());
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put(field, value.into());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put(field, value.into());
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.put(field, value.into());
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field, format!("{value:?}").into());
    }
}

#[cfg(test)]
mod tests {
    use super::{Format, Overrides, Rotation, file_layer, resolve};
    use crate::config::TelemetryConfig;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// `resolve` honours precedence: overrides beat config, and defaults fill the
    /// rest; an unset path with no `--log` means file logging is disabled.
    #[test]
    fn resolve_precedence_and_defaults() {
        // All unset ⇒ disabled, with the documented defaults for the other knobs.
        let s = resolve(&Overrides::default(), &TelemetryConfig::default()).expect("resolve");
        assert!(s.path.is_none(), "no file/flag ⇒ file logging off");
        assert_eq!(s.rotation, Rotation::Daily);
        assert_eq!(s.format, Format::Otel);

        // Config sets a path + knobs; an override for rotation wins over config.
        let cfg = TelemetryConfig {
            file: Some("/var/log/roteiro.log".to_owned()),
            rotation: Some("hourly".to_owned()),
            format: Some("json".to_owned()),
        };
        let over = Overrides {
            rotation: Some("never".to_owned()),
            ..Default::default()
        };
        let s = resolve(&over, &cfg).expect("resolve");
        assert_eq!(
            s.path.as_deref(),
            Some(std::path::Path::new("/var/log/roteiro.log"))
        );
        assert_eq!(s.rotation, Rotation::Never, "override beats config");
        assert_eq!(s.format, Format::Otel, "json is an alias of otel");

        // `--log-file` overrides a config path.
        let over = Overrides {
            file: Some("/tmp/explicit.log".to_owned()),
            ..Default::default()
        };
        let s = resolve(&over, &cfg).expect("resolve");
        assert_eq!(
            s.path.as_deref(),
            Some(std::path::Path::new("/tmp/explicit.log"))
        );

        // A bad rotation/format value is a hard error, never silently ignored.
        let bad = TelemetryConfig {
            rotation: Some("weekly".to_owned()),
            ..Default::default()
        };
        assert!(
            resolve(&Overrides::default(), &bad).is_err(),
            "bad rotation errors"
        );
    }

    /// The init seam, wired end-to-end: given a temp dir, an `info!` event lands in
    /// the rotating file as one OTEL-shaped JSON line carrying the expected fields.
    /// Uses a **scoped** subscriber (`with_default`) so it never races on the
    /// process-global default that other tests / `init` install.
    #[test]
    fn otel_file_layer_writes_parsable_line_with_otel_fields() {
        let dir = std::env::temp_dir().join(format!("roteiro-telemetry-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        // `never` rotation ⇒ the file is exactly this name (no date suffix), so the
        // test asserts a deterministic path. Rotation wiring itself is covered by
        // `Rotation::appender`; we do not assert on wall-clock rotation.
        let path = dir.join("roteiro.log");
        let (layer, guard) =
            file_layer(&path, Rotation::Never, Format::Otel).expect("build file layer");

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("ingest", repo = "demo");
            let _e = span.enter();
            tracing::info!(nodes = 42, "sync complete");
        });
        // Flush + join the non-blocking worker so the line is on disk before we read.
        drop(guard);

        let contents = std::fs::read_to_string(&path).expect("log file exists");
        let line = contents
            .lines()
            .find(|l| l.contains("sync complete"))
            .expect("our line");
        let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");

        // OTEL log data-model fields are present and correctly mapped.
        assert_eq!(v["body"], "sync complete", "message ⇒ OTEL body");
        assert_eq!(v["severity_text"], "INFO");
        assert_eq!(v["severity_number"], 9);
        assert!(
            v["time_unix_nano"].as_u64().is_some_and(|t| t > 0),
            "timestamp present as ns since epoch"
        );
        assert_eq!(
            v["attributes"]["nodes"], 42,
            "non-message field ⇒ attribute"
        );
        assert_eq!(
            v["attributes"]["span.name"], "ingest",
            "span context surfaced"
        );
        assert_eq!(v["resource"]["service.name"], "roteiro");

        std::fs::remove_dir_all(&dir).ok();
    }
}

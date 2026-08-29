//! Wall-clock timestamps for run evidence, in RFC 3339 UTC.
//!
//! An analyzer run's `started_at`/`ended_at`, an asset's `fetched_at` and a
//! rendered bundle's timestamps are **evidence about a point in time**, so
//! unlike everything on the extraction path they legitimately read a clock.
//! Nothing here feeds `nodes`/`edges`, so the determinism rule in `AGENTS.md` —
//! derived extraction is a pure function of `(path, blob id, bytes)` — is
//! untouched. Nothing in *this* module reads a clock either: every entry point
//! is a pure function of the `SystemTime` or the string it is handed, which is
//! what lets the callers that do read one stay testable.
//!
//! In **this** crate rather than in `rto-exec`, where it started, for the reason
//! `trust` and `model_choice` are here: everything depends on `rto-graph` and
//! `rto-exec` is optional. Until #667 these were `rto_exec::`, and a render path
//! — which is gated on nothing — reached across for one, so `cargo check -p
//! roteiro --no-default-features --features mcp` did not compile on `main` and
//! nobody found out, because no CI job built a configuration without
//! `execution`.
//!
//! It happened **twice**, which is why the fix is the move and not the call site.
//! The reference #667 reported was the workspace-vault renderer's
//! `generated_at`; #671 deleted that renderer and its OKF replacement reached for
//! `rfc3339_from_unix` in `okf_instant`, reproducing the same error at a new line
//! within days. Renderers want timestamps and are ungated; keeping the formatter
//! where they can always see it is what ends the class.
//!
//! `rto-exec` re-exports these four names, so its own callers did not move.
//!
//! The formatting is done by hand rather than by pulling a date-time crate: one
//! output format is needed (`YYYY-MM-DDTHH:MM:SSZ`), the civil-from-days
//! conversion is a dozen lines, and the project's dependency-light posture
//! (ADR-0001) makes a new crate for that a poor trade.
//!
//! NOT FOUND: `roteiro search "rfc3339 timestamp format"` and `roteiro search
//! "unix epoch seconds"` returned only `rto-serve`'s `unix_seconds`, which
//! yields a raw count for an OpenAI-compatible `created` field and formats
//! nothing. There was no existing formatter to reuse.

use std::time::{SystemTime, UNIX_EPOCH};

/// Format a [`SystemTime`] as RFC 3339 UTC with second granularity
/// (`2026-08-15T09:00:04Z`).
///
/// Times before the Unix epoch — a badly set clock, or a file stamped in 1969 —
/// are clamped to the epoch rather than rendered as a negative year, because a
/// negative year in an evidence record is noise, not information.
#[must_use]
pub fn rfc3339_utc(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .min(i64::MAX as u64);
    rfc3339_from_unix(secs)
}

/// Format `secs` seconds since the Unix epoch as RFC 3339 UTC.
///
/// ```
/// # use rto_graph::rfc3339_from_unix;
/// assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
/// assert_eq!(rfc3339_from_unix(1_755_248_400), "2025-08-15T09:00:00Z");
/// ```
#[must_use]
pub fn rfc3339_from_unix(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → `(year, month, day)`, by Howard Hinnant's
/// `civil_from_days` (public domain), which is exact for the whole proleptic
/// Gregorian range and needs no lookup tables.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Whole days between two RFC 3339 UTC timestamps, `None` if either will not
/// parse.
///
/// Used to report an advisory database's **age** — the number `roteiro security
/// status` prints and the reason a result is labelled *possibly stale* rather
/// than *current*.
#[must_use]
pub fn age_in_days(from: &str, to: &str) -> Option<i64> {
    Some((unix_from_rfc3339(to)? - unix_from_rfc3339(from)?) / 86_400)
}

/// Parse `YYYY-MM-DD` or a full RFC 3339 UTC timestamp to seconds since the
/// epoch. Deliberately strict and small: it accepts what [`rfc3339_from_unix`]
/// emits and the date-only form advisory databases publish, and refuses
/// anything else rather than guessing an offset.
#[must_use]
pub fn unix_from_rfc3339(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86_400;
    if bytes.len() >= 19 && (bytes[10] == b'T' || bytes[10] == b' ') {
        let hour: i64 = s.get(11..13)?.parse().ok()?;
        let minute: i64 = s.get(14..16)?.parse().ok()?;
        let second: i64 = s.get(17..19)?.parse().ok()?;
        if hour > 23 || minute > 59 || second > 60 {
            return None;
        }
        secs += hour * 3600 + minute * 60 + second;
        // An offset is *converted*, never ignored. `cargo audit --json` really
        // does stamp its database as `2026-08-12T12:42:29+02:00`, so reading the
        // offset as if it were `Z` would put an advisory database's age out by
        // hours — a staleness claim that is quietly wrong. Anything that is
        // neither `Z` nor a well-formed offset is refused instead of guessed.
        secs -= offset_seconds(&s[19..])?;
    } else if bytes.len() != 10 {
        return None;
    }
    Some(secs)
}

/// Seconds to subtract to turn a local time into UTC: `0` for `Z` or an absent
/// suffix, `+7200` for `+02:00`. `None` for anything else.
///
/// Fractional seconds are tolerated ahead of the offset, because RFC 3339
/// permits them and a producer that emits them is not wrong; they are dropped,
/// since this type has second granularity.
fn offset_seconds(suffix: &str) -> Option<i64> {
    let suffix = match suffix.split_once('.') {
        // Skip the fraction: digits up to the offset marker.
        Some((_, rest)) => rest.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => suffix,
    };
    if suffix.is_empty() || suffix == "Z" || suffix == "z" {
        return Some(0);
    }
    let sign = match suffix.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &suffix[1..];
    // Both `+02:00` and `+0200` occur in the wild.
    let (hours, minutes) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None if rest.len() == 4 => rest.split_at(2),
        None => return None,
    };
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// `(year, month, day)` → days since 1970-01-01, the inverse of
/// [`civil_from_days`].
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::{age_in_days, rfc3339_from_unix, rfc3339_utc, unix_from_rfc3339};
    use std::time::{Duration, UNIX_EPOCH};

    /// 2025-08-15T09:00:00Z, as seconds. Named so the `Duration` below is built
    /// from a constant rather than a literal clippy would rather see in hours.
    const SAMPLE: u64 = 1_755_248_400;

    #[test]
    fn formats_known_instants() {
        for (secs, text) in [
            (0_u64, "1970-01-01T00:00:00Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (951_782_400, "2000-02-29T00:00:00Z"), // a leap day in a leap century
            (1_755_248_400, "2025-08-15T09:00:00Z"),
            (4_102_444_800, "2100-01-01T00:00:00Z"), // 2100 is *not* a leap year
        ] {
            assert_eq!(rfc3339_from_unix(secs), text, "for {secs}");
        }
    }

    #[test]
    fn a_system_time_formats_the_same_way() {
        let at = UNIX_EPOCH + Duration::from_secs(SAMPLE);
        assert_eq!(rfc3339_utc(at), "2025-08-15T09:00:00Z");
        // A clock set before the epoch is clamped rather than rendered as a
        // negative year: an evidence record wants a usable value or none.
        let before = UNIX_EPOCH - Duration::from_secs(10);
        assert_eq!(rfc3339_utc(before), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn parsing_round_trips_formatting() {
        for secs in [0_u64, 1, 951_782_400, 1_755_248_400, 4_102_444_800] {
            let text = rfc3339_from_unix(secs);
            assert_eq!(
                unix_from_rfc3339(&text),
                Some(i64::try_from(secs).expect("in range")),
                "{text}"
            );
        }
    }

    #[test]
    fn accepts_the_date_only_form_advisory_databases_publish() {
        let secs = unix_from_rfc3339("2026-08-15").expect("a date-only timestamp parses");
        assert_eq!(secs, 1_786_752_000);
        // …and means midnight UTC on that date, not some other instant.
        assert_eq!(
            rfc3339_from_unix(u64::try_from(secs).expect("positive")),
            "2026-08-15T00:00:00Z"
        );
    }

    #[test]
    fn refuses_what_it_cannot_read_rather_than_guessing() {
        for bad in [
            "",
            "not a date",
            "2026/08/15",
            "2026-13-01",
            "2026-08-32",
            "2026-08-15T00:00",
            "2026-08-15T00:00:00+bad",
            "2026-08-15T00:00:00 UTC",
            "2026-08-15T00:00:00+99:00",
        ] {
            assert_eq!(unix_from_rfc3339(bad), None, "{bad:?}");
        }
        assert_eq!(age_in_days("2026-08-15", "not a date"), None);
    }

    /// `cargo audit --json` stamps its advisory database with a numeric offset
    /// (`2026-08-12T12:42:29+02:00`, observed from cargo-audit 0.22.2), so the
    /// offset has to be *converted*. Reading it as UTC would put every staleness
    /// age out by the offset.
    #[test]
    fn converts_a_numeric_offset_rather_than_ignoring_it() {
        let utc = unix_from_rfc3339("2026-08-12T10:42:29Z").expect("utc");
        assert_eq!(unix_from_rfc3339("2026-08-12T12:42:29+02:00"), Some(utc));
        assert_eq!(unix_from_rfc3339("2026-08-12T12:42:29+0200"), Some(utc));
        assert_eq!(unix_from_rfc3339("2026-08-12T08:42:29-02:00"), Some(utc));
        // Fractional seconds are permitted by RFC 3339 and dropped here.
        assert_eq!(unix_from_rfc3339("2026-08-12T10:42:29.512Z"), Some(utc));
    }

    #[test]
    fn reports_age_in_whole_days() {
        assert_eq!(
            age_in_days("2026-08-01T00:00:00Z", "2026-08-15T00:00:00Z"),
            Some(14)
        );
        assert_eq!(age_in_days("2026-08-15", "2026-08-15T23:00:00Z"), Some(0));
        assert_eq!(age_in_days("2026-08-15", "bad"), None);
    }
}

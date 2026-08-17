//! The egress ledger — *"what left this machine, and when?"*, answerable after
//! the fact rather than reconstructed.
//!
//! ADR-0019's consequences require that **every remote call is recorded**:
//! endpoint, model string, [`ProducerTrust`], timestamp. This adds the payload
//! itself, because §4 asks for "a recorded copy of what did" leave, and a record
//! of *that a call happened* answers a much weaker question than a record of
//! *what it carried*.
//!
//! # Two lines per call, and the order matters
//!
//! An [`Entry::Egress`] line is written **before** the transport is invoked, and
//! an [`Entry::Outcome`] line after it returns. A single line written afterwards
//! would lose exactly the calls worth knowing about — the one that hung, the one
//! that panicked, the one the machine died during — and "we have no record, so
//! presumably nothing left" is the wrong default for an egress log.
//!
//! It follows that an unwritable ledger **refuses the call** rather than sending
//! unrecorded; see [`crate::call_with`].
//!
//! # It is append-only, and it is not a graph fact
//!
//! Entries are appended as JSON Lines and never rewritten. Nothing here becomes
//! a node, an edge, a `Provenance` variant, or part of `export_factset` — the
//! fourth application of the rule ADR-0012, ADR-0013 and ADR-0015 already
//! established for analyzer findings, agent memory and generated media.
//!
//! # It holds sensitive bytes, and is created accordingly
//!
//! A recorded payload carries whatever the payload carried — symbol names,
//! paths, captured prose. On Unix the file is created `0600`, because a log of
//! everything a repository disclosed is not something to leave world-readable by
//! inheriting a umask.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::trust::ProducerTrust;

/// Distinguishes calls that share a timestamp. Per-process and monotonic; the
/// timestamp is what orders entries across processes.
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// A handle to one machine's egress ledger.
///
/// It owns a path and nothing else: no cached file handle, no buffering, no
/// background writer. Each [`Ledger::append`] opens, writes one line, flushes and
/// closes, so a record is on disk before the call it describes is made, and a
/// crash cannot lose a buffered egress line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    path: PathBuf,
}

/// Why the ledger could not be read or written.
///
/// One variant, deliberately: every failure here has the same consequence —
/// a call that cannot be recorded is a call that does not happen — so splitting
/// the cause into variants would buy a match arm nobody would branch on.
#[derive(Debug, thiserror::Error)]
#[error("the remote-call ledger at {path} could not be {action}: {source}")]
pub struct LedgerError {
    /// The ledger's path.
    pub path: PathBuf,
    /// `written` or `read`.
    pub action: &'static str,
    /// The underlying I/O failure.
    #[source]
    pub source: std::io::Error,
}

/// One line of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Entry {
    /// Bytes are about to leave. Written *before* the transport is invoked.
    Egress(Egress),
    /// What came back, or what went wrong. Written after.
    Outcome(Outcome),
}

impl Entry {
    /// The call id both lines of a call share.
    #[must_use]
    pub fn call(&self) -> &str {
        match self {
            Self::Egress(e) => &e.call,
            Self::Outcome(o) => &o.call,
        }
    }

    /// When this line was written.
    #[must_use]
    pub fn at(&self) -> &str {
        match self {
            Self::Egress(e) => &e.at,
            Self::Outcome(o) => &o.at,
        }
    }
}

/// The record of bytes leaving: every field ADR-0019 names, plus the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Egress {
    /// Identifies this call; the matching [`Outcome`] repeats it.
    pub call: String,
    /// RFC 3339 UTC, from the caller's clock.
    pub at: String,
    /// The URL the bytes went to.
    pub endpoint: String,
    /// The model string asked for.
    pub model: String,
    /// Whether that model string identifies anything verifiable. Always
    /// [`ProducerTrust::VendorAsserted`] for a hosted model.
    pub trust: ProducerTrust,
    /// Which classes of information the payload carried, in the payload's own
    /// words ([`crate::Payload::fields_present`]).
    pub fields: Vec<String>,
    /// The size of the body in bytes.
    pub bytes: usize,
    /// The body, verbatim — the recorded copy of what left.
    pub body: String,
}

/// What the call returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The [`Egress`] line this closes.
    pub call: String,
    /// RFC 3339 UTC, from the caller's clock.
    pub at: String,
    /// Whether the transport reported success.
    pub ok: bool,
    /// The transport's own words on failure; empty on success.
    pub detail: String,
    /// The size of the response in bytes; `0` on failure.
    pub response_bytes: usize,
}

impl Ledger {
    /// A ledger at `path`. Creates nothing until something is appended.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where this ledger lives — printed by `roteiro remote status` so an
    /// operator can read it with their own tools.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A call id, unique within this machine's ledger: the timestamp plus a
    /// per-process counter, so two calls in the same second are still
    /// distinguishable and the id sorts the way the file does.
    #[must_use]
    pub fn next_call_id(at: &str) -> String {
        format!("{at}#{:06}", SEQUENCE.fetch_add(1, Ordering::Relaxed))
    }

    /// Append one entry, creating the file (and its directory) if needed.
    ///
    /// Returns only once the line is flushed, because the caller uses that as
    /// permission to send.
    ///
    /// # Errors
    /// [`LedgerError`] if the directory or file cannot be created or written.
    pub fn append(&self, entry: &Entry) -> Result<(), LedgerError> {
        let fail = |source: std::io::Error| LedgerError {
            path: self.path.clone(),
            action: "written",
            source,
        };
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(fail)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // Owner-only: this file holds a copy of everything the repository
            // disclosed. The mode applies at creation; an existing file keeps
            // whatever it has, which is the operator's business.
            options.mode(0o600);
        }
        let mut file = options.open(&self.path).map_err(fail)?;
        // Serializing owned data cannot fail; an I/O error can, and does not get
        // swallowed.
        let line = serde_json::to_string(entry).unwrap_or_default();
        file.write_all(line.as_bytes()).map_err(fail)?;
        file.write_all(b"\n").map_err(fail)?;
        file.flush().map_err(fail)
    }

    /// Every entry, oldest first. A missing ledger reads as empty — nothing has
    /// left this machine yet, which is a true and useful answer.
    ///
    /// A line that will not parse is **skipped rather than fatal**: a truncated
    /// last line from a killed process must not make the whole history
    /// unreadable, and the entries before it are exactly the ones being asked
    /// about.
    ///
    /// # Errors
    /// [`LedgerError`] if the file exists but cannot be read.
    pub fn read(&self) -> Result<Vec<Entry>, LedgerError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(LedgerError {
                    path: self.path.clone(),
                    action: "read",
                    source,
                });
            }
        };
        Ok(std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Egress, Entry, Ledger, Outcome};
    use crate::trust::ProducerTrust;

    fn egress(call: &str) -> Entry {
        Entry::Egress(Egress {
            call: call.to_owned(),
            at: "2026-08-17T09:00:00Z".to_owned(),
            endpoint: "https://models.example/v1/chat/completions".to_owned(),
            model: "a-vendor-model".to_owned(),
            trust: ProducerTrust::VendorAsserted,
            fields: vec!["instruction".to_owned()],
            bytes: 42,
            body: "{\"model\":\"a-vendor-model\"}".to_owned(),
        })
    }

    /// An egress line carries every field ADR-0019 names — endpoint, model,
    /// trust, timestamp — plus the copy of what left. Asserted against the
    /// **serialized** form, because the file is the artifact and a field that
    /// exists in Rust but not on disk answers nothing later.
    #[test]
    fn an_egress_line_carries_endpoint_model_trust_and_timestamp() {
        let json = serde_json::to_string(&egress("c1")).expect("serialize");
        for needle in [
            "\"event\":\"egress\"",
            "\"endpoint\":\"https://models.example/v1/chat/completions\"",
            "\"model\":\"a-vendor-model\"",
            "\"trust\":\"vendor_asserted\"",
            "\"at\":\"2026-08-17T09:00:00Z\"",
            "\"body\":",
        ] {
            assert!(json.contains(needle), "missing {needle} in {json}");
        }
    }

    /// The ledger appends and round-trips, and a call's two lines are tied
    /// together by a shared id — which is what makes "did this call return?"
    /// answerable rather than guessable.
    #[test]
    fn the_ledger_appends_and_round_trips() {
        let dir = crate::testing::temp_dir("ledger-append");
        // A nested path, so the directory creation is exercised too.
        let ledger = Ledger::at(dir.join("remote").join("egress.jsonl"));
        assert!(ledger.read().expect("a missing ledger is empty").is_empty());

        ledger.append(&egress("c1")).expect("append");
        ledger
            .append(&Entry::Outcome(Outcome {
                call: "c1".to_owned(),
                at: "2026-08-17T09:00:01Z".to_owned(),
                ok: true,
                detail: String::new(),
                response_bytes: 7,
            }))
            .expect("append");
        ledger.append(&egress("c2")).expect("append");

        let entries = ledger.read().expect("read");
        assert_eq!(entries.len(), 3, "append-only, oldest first");
        assert_eq!(entries[0].call(), "c1");
        assert_eq!(entries[1].call(), "c1", "the outcome closes its own egress");
        assert_eq!(entries[2].call(), "c2");
        assert_eq!(entries[1].at(), "2026-08-17T09:00:01Z");
        assert!(matches!(entries[0], Entry::Egress(_)));
        assert!(matches!(entries[1], Entry::Outcome(_)));
    }

    /// A truncated final line — a process killed mid-write — must not make the
    /// history before it unreadable. The whole point of the ledger is answering
    /// what left *earlier*.
    #[test]
    fn a_truncated_line_does_not_hide_the_history_before_it() {
        let dir = crate::testing::temp_dir("ledger-truncated");
        let path = dir.join("egress.jsonl");
        let ledger = Ledger::at(&path);
        ledger.append(&egress("c1")).expect("append");
        std::fs::write(
            &path,
            format!(
                "{}\n{{\"event\":\"egr",
                serde_json::to_string(&egress("c1")).expect("serialize")
            ),
        )
        .expect("write");

        let entries = ledger.read().expect("read");
        assert_eq!(entries.len(), 1, "the intact line survives");
        assert_eq!(entries[0].call(), "c1");
    }

    /// Call ids are distinct within a second, so two calls that share a
    /// timestamp are still two calls in the record.
    #[test]
    fn call_ids_distinguish_calls_that_share_a_timestamp() {
        let at = "2026-08-17T09:00:00Z";
        let a = Ledger::next_call_id(at);
        let b = Ledger::next_call_id(at);
        assert_ne!(a, b);
        assert!(a.starts_with(at) && b.starts_with(at), "{a} {b}");
        assert!(a < b, "ids sort in call order: {a} then {b}");
    }

    /// On Unix a ledger holding copies of everything a repository disclosed is
    /// created owner-only rather than inheriting a umask.
    #[cfg(unix)]
    #[test]
    fn the_ledger_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = crate::testing::temp_dir("ledger-mode");
        let path = dir.join("egress.jsonl");
        Ledger::at(&path).append(&egress("c1")).expect("append");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }
}

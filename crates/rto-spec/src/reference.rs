//! The **reference register** — the authored list of external works this
//! repository cites (ADR-0029, issue #801).
//!
//! One checked-in TOML file, [`REGISTER_PATH`], holds an array of `[[reference]]`
//! tables, each deserialised straight into an [`rto_graph::Reference`]. The
//! record type is the schema: nothing here re-describes its fields, so the
//! register cannot accept a shape the renderer has never heard of.
//!
//! Two things come out of the authored pass:
//!
//! - every valid entry becomes a `work` node, `authored`, carrying its record
//!   under [`rto_graph::WORK_RECORD`];
//! - every external inline link in an authored markdown document whose
//!   destination is a work's URL becomes a [`Citation`], which `crate::check`
//!   turns into an `authored` `references` edge from the document to the work.
//!
//! An entry that does not parse is a [`ViolationKind::MalformedReference`]
//! naming it — never a silent skip, because a register that quietly drops an
//! entry renders a reference list that is quietly short.

use std::collections::BTreeMap;

use rto_graph::{
    AccessDate, FactSet, Locator, Node, NodeKind, Provenance, PublicationDate, Reference,
};
use serde::{Deserialize, Serialize};

use crate::check::{Violation, ViolationKind};

/// Where the register lives. A path rule, like `docs/adr/`.
pub const REGISTER_PATH: &str = "docs/references.toml";

/// One external link in an authored document — a candidate citation of a work.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Citation {
    /// Node key of the document the link appears in.
    pub from: String,
    /// The link destination, exactly as written.
    pub url: String,
}

/// Parse the register's text into its records, and a violation for every entry
/// (or the whole file) that cannot be one.
///
/// Entries are parsed one at a time so a bad one names itself and the good ones
/// still load. Ids must be non-empty, carry no surrounding whitespace, and be
/// unique: an id is a node key and the thing a refusal names.
#[must_use]
pub fn parse_register(path: &str, text: &str) -> (Vec<Reference>, Vec<Violation>) {
    let malformed = |message: String| Violation {
        kind: ViolationKind::MalformedReference,
        message: format!("{path}: {message}"),
    };
    let mut table = match text.parse::<toml::Table>() {
        Ok(table) => table,
        Err(e) => return (Vec::new(), vec![malformed(format!("cannot parse: {e}"))]),
    };
    let mut violations = Vec::new();
    let entries = match table.remove("reference") {
        None => Vec::new(),
        Some(toml::Value::Array(entries)) => entries,
        Some(_) => {
            violations.push(malformed(
                "`reference` must be an array of tables (`[[reference]]`)".to_owned(),
            ));
            Vec::new()
        }
    };
    // Proposals are validated here so a hand-edited one cannot rot unnoticed,
    // and never returned: they are not entries (ADR-0029 §7).
    let proposals = table.remove("proposal");
    for key in table.keys() {
        violations.push(malformed(format!(
            "unknown top-level key `{key}` (the register holds `[[reference]]` entries \
             and `[[proposal]]`s)"
        )));
    }

    let mut records = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let position = index + 1;
        let label = entry.get("id").and_then(toml::Value::as_str).map_or_else(
            || format!("entry {position}"),
            |id| format!("entry {position} (`{id}`)"),
        );
        let record: Reference = match entry.try_into() {
            Ok(record) => record,
            Err(e) => {
                violations.push(malformed(format!("{label}: {}", e.message())));
                continue;
            }
        };
        if record.id.is_empty() || record.id.trim() != record.id {
            violations.push(malformed(format!(
                "{label}: `id` must be non-empty with no surrounding whitespace"
            )));
            continue;
        }
        if let Some(first) = seen.insert(record.id.clone(), position) {
            violations.push(malformed(format!(
                "{label}: duplicate id, first used by entry {first}"
            )));
            continue;
        }
        records.push(record);
    }
    if let Some(value) = proposals {
        let mut only = toml::Table::new();
        only.insert("proposal".to_owned(), value);
        match parse_proposals(&only.to_string()) {
            Ok(proposals) => {
                for p in proposals {
                    if !seen.contains_key(&p.id) {
                        violations.push(malformed(format!(
                            "proposal for `{}` names no register entry",
                            p.id
                        )));
                    }
                    if !p.retrieved.names_a_day_that_exists() {
                        violations.push(malformed(format!(
                            "proposal for `{}` was retrieved on a day that does not exist",
                            p.id
                        )));
                    }
                }
            }
            Err(e) => violations.push(malformed(e)),
        }
    }
    (records, violations)
}

/// Whether a fetched page stated a value.
///
/// Its own two-state type rather than an `Option` or an
/// [`rto_graph::Attested`]: `NotStated` is a fact about the **page that was
/// read**, which is neither "nobody looked" (`Unknown`) nor "the work has none"
/// (`AbsentFromWork`). Moving it into an entry is a human's call, and the
/// type makes them choose which of those it becomes.
///
/// Deliberately not `#[non_exhaustive]`: a page either stated a value or did
/// not, and a third answer would be a different question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Found<T> {
    /// The page stated this value.
    Stated(T),
    /// The page was read and stated none.
    NotStated,
}

/// Values `roteiro refs fetch` read from a work's locator (ADR-0029 §7) — a
/// **proposal** awaiting review, never an entry.
///
/// Kept in the register file under `[[proposal]]`, below a marker the tool owns,
/// and never applied to the graph: a scraped title is not authored fact
/// (issue #801, constraint 2), so it stays out of every rendered list until a
/// human moves it into the entry above.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    /// The register entry this proposes values for.
    pub id: String,
    /// The URL that was read.
    pub url: String,
    /// When it was read — an access date, and only ever that.
    pub retrieved: AccessDate,
    /// The title the page states.
    pub title: Found<String>,
    /// The publication date the page's metadata states, at its precision.
    pub published: Found<PublicationDate>,
    /// The version the page's metadata states.
    pub version: Found<String>,
}

/// The line above which the register belongs to people and below which
/// `roteiro refs fetch` rewrites its proposals.
pub const PROPOSALS_MARKER: &str =
    "# ---- proposals: written by `roteiro refs fetch`; everything below is regenerated ----";

/// The `[[proposal]]` array of a register, as TOML holds it.
#[derive(Serialize, Deserialize)]
struct ProposalTable {
    #[serde(default)]
    proposal: Vec<Proposal>,
}

/// The `[[proposal]]` tables in a register's text.
///
/// # Errors
/// A message naming the problem when the text or a proposal does not parse.
pub fn parse_proposals(text: &str) -> Result<Vec<Proposal>, String> {
    let mut table = text.parse::<toml::Table>().map_err(|e| e.to_string())?;
    let mut only = toml::Table::new();
    if let Some(value) = table.remove("proposal") {
        only.insert("proposal".to_owned(), value);
    }
    toml::Value::Table(only)
        .try_into::<ProposalTable>()
        .map(|t| t.proposal)
        .map_err(|e| format!("`[[proposal]]`: {}", e.message()))
}

/// `text` with its proposals section replaced by `proposals` — or removed when
/// there are none.
///
/// Everything above [`PROPOSALS_MARKER`] is returned byte-for-byte: the
/// register's entries and comments are a person's, and a tool that re-serialised
/// them would discard the comments recording why each value is what it is.
///
/// # Errors
/// The serialiser's message if a proposal cannot be written as TOML.
pub fn write_proposals(text: &str, proposals: &[Proposal]) -> Result<String, String> {
    let head = text
        .find(PROPOSALS_MARKER)
        .map_or(text, |at| &text[..at])
        .trim_end();
    if proposals.is_empty() {
        return Ok(format!("{head}\n"));
    }
    let body = toml::to_string(&ProposalTable {
        proposal: proposals.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "{head}\n\n{PROPOSALS_MARKER}\n# Fetched, not attested. Check each value against the work, \
         move what is right into\n# its entry above, and delete the proposal.\n\n{body}"
    ))
}

/// The URL a work is found at, if it has one — the string a document's link is
/// matched against.
#[must_use]
pub fn locator_url(reference: &Reference) -> Option<&str> {
    match reference.locator.known()? {
        Locator::Url(url) | Locator::Both { url, .. } => Some(url),
        Locator::Doi(_) => None,
    }
}

/// The `work` node for one register entry.
///
/// `meta.content` is the title and URL, so `roteiro search` finds a work by
/// either; `meta.reference` is the whole record, so every read surface renders
/// from the graph rather than re-reading the register.
#[must_use]
pub fn work_facts(reference: &Reference) -> FactSet {
    let mut node = Node::new(
        rto_graph::work_key(&reference.id),
        NodeKind::Work,
        reference.title.clone(),
    )
    .with_provenance(Provenance::Authored);
    node.path = Some(REGISTER_PATH.to_owned());
    let content = match locator_url(reference) {
        Some(url) => format!("{}\n{url}", reference.title),
        None => reference.title.clone(),
    };
    node.meta = serde_json::json!({
        "content": content,
        rto_graph::WORK_RECORD: reference,
    });
    FactSet::new().with_node(node)
}

/// Every external inline link and autolink in markdown `text`, attributed to
/// `from`.
///
/// Reads through [`rto_graph::markdown_links`], the one link scanner, and skips
/// fenced code, where a link is an example rather than a citation. Wiki-links
/// and images are not citations; internal links name this repository. An
/// autolink (`<https://…>`) cites exactly as `[text](https://…)` does — it is
/// how ADR-0021 and ADR-0022 cite the OKF specification.
#[must_use]
pub fn citations(from: &str, text: &str) -> Vec<Citation> {
    let mut in_fence = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if rto_graph::is_code_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        out.extend(
            rto_graph::markdown_links(line)
                .into_iter()
                .filter(|l| {
                    matches!(
                        l.kind(),
                        rto_graph::LinkKind::Inline | rto_graph::LinkKind::Autolink
                    ) && l.scope().is_external()
                })
                .map(|l| Citation {
                    from: from.to_owned(),
                    url: l.target().to_owned(),
                }),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{citations, parse_register};
    use crate::check::ViolationKind;
    use rto_graph::{Attested, Locator, PublicationDate, Stability, WorkKind};

    const SPEC: &str = r#"
[[reference]]
id = "okf-spec"
kind = "document"
authors = [{ group = "Google Cloud" }]
published = { known = { full = { year = 2026, month = "june", day = 12 } } }
title = "Open Knowledge Format"
version = { known = "0.2" }
descriptor = "absent-from-work"
publisher = { known = "Google Cloud" }
locator = { known = { url = "https://example.org/SPEC.md" } }
stability = "fixed-or-archived"
"#;

    #[test]
    fn a_register_entry_is_a_reference_record() {
        let (records, violations) = parse_register("r.toml", SPEC);
        assert!(violations.is_empty(), "{violations:?}");
        let r = &records[0];
        assert_eq!(r.id, "okf-spec");
        assert_eq!(r.kind, WorkKind::Document);
        assert!(matches!(
            r.published,
            Attested::Known(PublicationDate::Full { .. })
        ));
        assert_eq!(
            r.locator,
            Attested::Known(Locator::Url("https://example.org/SPEC.md".into()))
        );
        assert_eq!(r.stability, Stability::FixedOrArchived);
    }

    /// An invalid entry is named, and does not take the valid ones with it.
    #[test]
    fn a_bad_entry_is_a_violation_naming_it_and_the_rest_still_load() {
        let bad = SPEC
            .replace("day = 12", "day = 32")
            .replace("okf-spec", "broken");
        let text = format!("{SPEC}\n{bad}\n[[reference]]\nid = \"okf-spec\"\n");
        let (records, violations) = parse_register("r.toml", &text);
        assert_eq!(records.len(), 1, "the valid entry still loads");
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations
                .iter()
                .all(|v| v.kind == ViolationKind::MalformedReference)
        );
        assert!(
            violations[0].message.contains("entry 2 (`broken`)"),
            "{violations:?}"
        );
        assert!(
            violations[1].message.contains("entry 3 (`okf-spec`)"),
            "{violations:?}"
        );
    }

    #[test]
    fn duplicate_ids_unknown_fields_and_unknown_tables_are_refused() {
        let text = format!("{SPEC}{SPEC}");
        let (records, violations) = parse_register("r.toml", &text);
        assert_eq!(records.len(), 1);
        assert!(
            violations[0].message.contains("duplicate id"),
            "{violations:?}"
        );

        let extra = SPEC.replace("kind = ", "notes = \"x\"\nkind = ");
        let (records, violations) = parse_register("r.toml", &extra);
        assert!(records.is_empty());
        assert!(violations[0].message.contains("notes"), "{violations:?}");

        let (_, violations) = parse_register("r.toml", "[[referenc]]\nid = \"x\"\n");
        assert!(violations[0].message.contains("referenc"), "{violations:?}");

        let (_, violations) = parse_register("r.toml", "not toml = = =");
        assert!(
            violations[0].message.contains("cannot parse"),
            "{violations:?}"
        );
    }

    fn proposal(id: &str) -> super::Proposal {
        super::Proposal {
            id: id.to_owned(),
            url: "https://example.org/SPEC.md".to_owned(),
            retrieved: rto_graph::AccessDate {
                year: rto_graph::Year::new(2026).expect("year"),
                month: rto_graph::Month::November,
                day: rto_graph::Day::new(11).expect("day"),
            },
            title: super::Found::Stated("Open Knowledge Format".to_owned()),
            published: super::Found::NotStated,
            version: super::Found::NotStated,
        }
    }

    /// The human half of the register survives byte-for-byte; the proposals
    /// round-trip, replace themselves rather than accumulate, and stay valid.
    #[test]
    fn proposals_are_written_below_the_marker_and_never_touch_the_entries() {
        let head = format!("# a comment a person wrote\n{SPEC}");
        let once = super::write_proposals(&head, &[proposal("okf-spec")]).expect("write");
        assert!(once.starts_with(head.trim_end()), "{once}");
        assert_eq!(
            super::parse_proposals(&once).expect("parse"),
            vec![proposal("okf-spec")]
        );
        let (records, violations) = parse_register("r.toml", &once);
        assert!(violations.is_empty(), "{violations:?}");
        assert_eq!(records.len(), 1, "a proposal is not an entry");

        let twice = super::write_proposals(&once, &[proposal("okf-spec")]).expect("write");
        assert_eq!(once, twice, "rewriting is idempotent");
        assert_eq!(
            super::write_proposals(&once, &[]).expect("write"),
            format!("{}\n", head.trim_end())
        );

        let orphan = super::write_proposals(&head, &[proposal("nobody")]).expect("write");
        let (_, violations) = parse_register("r.toml", &orphan);
        assert!(violations[0].message.contains("`nobody`"), "{violations:?}");
    }

    #[test]
    fn citations_are_external_inline_links_outside_fences() {
        let text = "See [the spec](https://example.org/SPEC.md) and [ours](docs/x.md).\n\
                    [[docs/y.md]] ![img](https://example.org/i.png)\n\
                    ```\n[example](https://example.org/fenced)\n```\n";
        let found: Vec<String> = citations("site:a", text)
            .into_iter()
            .map(|c| c.url)
            .collect();
        assert_eq!(found, vec!["https://example.org/SPEC.md"]);
    }

    /// ADR-0021's form: `- The specification: <https://…>`. It made no edge
    /// before autolinks were read (ADR-0029, Consequences).
    #[test]
    fn an_autolink_is_a_citation() {
        let text = "- The specification: <https://example.org/SPEC.md> (v0.2)\n";
        let found: Vec<String> = citations("adr:0021", text)
            .into_iter()
            .map(|c| c.url)
            .collect();
        assert_eq!(found, vec!["https://example.org/SPEC.md"]);
    }
}

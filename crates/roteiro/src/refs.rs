//! `roteiro refs` — the APA 7 reference list of the external works this
//! repository cites (ADR-0029, issue #801).
//!
//! The works are `work` nodes the authored layer built from
//! `docs/references.toml`; this reads them from the graph and renders them with
//! [`rto_render::apa`], which refuses an incomplete record rather than guess at
//! it. A refusal is printed, with the fields that stopped it — it is the
//! register's worklist, and dropping it would make the list look complete.

use rto_graph::Reference;
use rto_render::apa::{self, ReferenceList};
use serde::Serialize;

/// `refs --json`: which node was asked about (none for the whole register), and
/// the reference list.
#[derive(Serialize)]
struct Report<'a> {
    key: Option<&'a str>,
    #[serde(flatten)]
    list: ReferenceList,
}

/// Run `roteiro refs [<key>] [--json]`.
pub(crate) fn run(
    ingest: rto_graph::IngestConfig,
    key: Option<&str>,
    json: bool,
    source: crate::SourceArgs,
) -> anyhow::Result<()> {
    let (repo, mut store, cache) = crate::open_graph()?;
    if crate::refresh_for_read(&repo, &mut store, &cache, ingest, source.source())?
        == crate::Refreshed::Yes
    {
        source.announce();
    }
    let works = match key {
        Some(key) => {
            anyhow::ensure!(
                store.get_node(key)?.is_some(),
                "no node with key `{key}` (try `roteiro search` to find one)"
            );
            rto_graph::works_cited(&store, key)?
        }
        None => rto_graph::works(&store)?,
    };
    let list = apa::reference_list(&works);
    if json {
        crate::emit_json(&Report { key, list })?;
    } else if works.is_empty() {
        match key {
            Some(key) => println!("{key} cites no work in the reference register"),
            None => println!("the reference register is empty (docs/references.toml)"),
        }
    } else {
        for line in lines(&list) {
            println!("{line}");
        }
    }
    Ok(())
}

/// Print the works a node rests on under `query`/`context`'s human output, and
/// nothing when it cites none.
pub(crate) fn print_cited(works: &[Reference]) {
    if works.is_empty() {
        return;
    }
    println!("  references:");
    for line in lines(&apa::reference_list(works)) {
        println!("    {line}");
    }
}

/// The human form of a reference list: the entries, then everything that could
/// not be cited or told apart. Split out from the printing so it is testable.
fn lines(list: &ReferenceList) -> Vec<String> {
    let mut out = Vec::new();
    if !list.entries.is_empty() {
        out.push("References (APA 7):".to_owned());
        out.extend(list.entries.iter().map(|e| format!("  {}", e.plain_text())));
    }
    if !list.refused.is_empty() {
        out.push("Not cited — the record is incomplete, and nothing is guessed:".to_owned());
        out.extend(list.refused.iter().map(|r| format!("  {r}")));
    }
    if !list.ambiguous.is_empty() {
        out.push("Ambiguous — these share one in-text citation:".to_owned());
        out.extend(
            list.ambiguous
                .iter()
                .map(|a| format!("  {}: {}", a.citation, a.reference_ids.join(", "))),
        );
    }
    if !list.duplicate_ids.is_empty() {
        out.push(format!("Duplicate ids: {}", list.duplicate_ids.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::lines;
    use rto_graph::{
        Attested, Author, Day, Locator, Month, PublicationDate, Reference, Stability, WorkKind,
        Year,
    };
    use rto_render::apa::reference_list;

    fn citable(id: &str, title: &str) -> Reference {
        let mut r = Reference::new(id, WorkKind::Document, title, Stability::FixedOrArchived);
        r.authors = vec![Author::Group("Google Cloud".into())];
        r.published = Attested::Known(PublicationDate::Full {
            year: Year::new(2026).expect("year"),
            month: Month::June,
            day: Day::new(12).expect("day"),
        });
        r.version = Attested::Known("0.2".into());
        r.descriptor = Attested::AbsentFromWork;
        r.publisher = Attested::Known("Google Cloud".into());
        r.locator = Attested::Known(Locator::Url("https://example.org/SPEC.md".into()));
        r
    }

    /// A refused record is printed with its reason, and an ambiguity is named —
    /// neither is dropped to make the list look complete.
    #[test]
    fn refusals_and_ambiguities_are_printed_not_dropped() {
        let unknown_date = Reference {
            published: Attested::Unknown,
            ..citable("undated", "Other")
        };
        let list = reference_list(&[
            citable("a", "Open Knowledge Format"),
            citable("b", "Another one"),
            unknown_date,
        ]);
        let text = lines(&list).join("\n");
        assert!(
            text.contains(
                "Google Cloud. (2026, June 12). Open Knowledge Format (Version 0.2). \
                 https://example.org/SPEC.md"
            ),
            "{text}"
        );
        assert!(
            text.contains("cannot cite undated: missing publication-date"),
            "{text}"
        );
        assert!(text.contains("(Google Cloud, 2026): b, a"), "{text}");
    }
}

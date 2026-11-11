//! `roteiro refs fetch` — read a register work's locator once, and write what
//! the page states into `docs/references.toml` as a **proposal** for a human to
//! review (ADR-0029 §7, issue #801).
//!
//! Shaped like `security prefetch`: an explicit subcommand, behind its own
//! feature, that opens a socket only with `--allow-download`, given every time.
//! Nothing here runs during a scan or a render, and nothing it writes reaches
//! the graph: a proposal is not an entry, so `roteiro refs` keeps rendering the
//! register as a human left it until somebody moves a value across.
//!
//! What it records, and what it refuses to infer:
//!
//! - the date it read the page, as an [`AccessDate`] — never as a publication
//!   date;
//! - a publication date only when the page states one in its metadata
//!   (`citation_publication_date`, `article:published_time`, `DC.date.issued`,
//!   …), and `not-stated` when it does not — no guess from a copyright line, a
//!   commit, or the response's `Last-Modified`;
//! - a title, and a version only from explicit metadata (`citation_version`,
//!   `softwareVersion`), likewise `not-stated` otherwise.

use rto_graph::{AccessDate, Day, Locator, Month, PublicationDate, Reference, Year};
use rto_spec::{Found, Proposal};

/// The most of a page this reads. Metadata lives in the `<head>`; a page larger
/// than this is read up to the limit rather than buffered whole.
const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024;

/// Run `roteiro refs fetch <id> | --all --allow-download`.
pub(crate) fn run(id: Option<&str>, all: bool, allow_download: bool) -> anyhow::Result<()> {
    // Consent first, before anything is read: without it this command does not
    // reach the network, and says how to let it.
    anyhow::ensure!(
        allow_download,
        "`roteiro refs fetch` reads each work's URL over the network, so it needs \
         --allow-download every time. Nothing was fetched."
    );
    anyhow::ensure!(id.is_some() != all, "name one register id, or pass --all");
    let (repo, _store, _cache) = crate::open_graph()?;
    let root = repo.workdir().ok_or_else(|| {
        anyhow::anyhow!("`refs fetch` needs a working tree to write the register in")
    })?;
    let path = root.join(rto_spec::REGISTER_PATH);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let (records, violations) = rto_spec::parse_register(rto_spec::REGISTER_PATH, &text);
    if let Some(first) = violations.first() {
        anyhow::bail!(
            "the register has {} problem(s); fix them before fetching into it — first: {}",
            violations.len(),
            first.message
        );
    }
    let chosen: Vec<&Reference> = match id {
        Some(id) => vec![
            records
                .iter()
                .find(|r| r.id == id)
                .ok_or_else(|| anyhow::anyhow!("no register entry has id `{id}`"))?,
        ],
        None => records.iter().collect(),
    };

    let today = today()?;
    let mut proposals = rto_spec::parse_proposals(&text).map_err(anyhow::Error::msg)?;
    let mut failed = 0usize;
    for reference in chosen {
        let Some(url) = locator_url(reference) else {
            println!("{}: no locator recorded, nothing to fetch", reference.id);
            continue;
        };
        match propose(&reference.id, &url, today) {
            Ok(proposal) => {
                println!("{}", summary(&proposal));
                proposals.retain(|p| p.id != proposal.id);
                proposals.push(proposal);
            }
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", reference.id);
            }
        }
    }
    proposals.sort_by(|a, b| a.id.cmp(&b.id));
    let updated = rto_spec::write_proposals(&text, &proposals).map_err(anyhow::Error::msg)?;
    std::fs::write(&path, updated)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    println!(
        "proposals written to {} — review them, move what is right into its entry, \
         and delete the proposal",
        rto_spec::REGISTER_PATH
    );
    anyhow::ensure!(failed == 0, "{failed} work(s) could not be fetched");
    Ok(())
}

/// The URL to read for a work: its URL, or its DOI's resolver URL.
fn locator_url(reference: &Reference) -> Option<String> {
    match reference.locator.known()? {
        Locator::Url(url) | Locator::Both { url, .. } => Some(url.clone()),
        Locator::Doi(doi) => Some(doi.url()),
    }
}

/// Today's date in UTC — the access date. The one clock read in this feature,
/// and it is recorded as exactly what it is.
fn today() -> anyhow::Result<AccessDate> {
    let stamp = rto_graph::rfc3339_utc(std::time::SystemTime::now());
    match iso_date(&stamp) {
        Some(PublicationDate::Full { year, month, day }) => Ok(AccessDate { year, month, day }),
        _ => anyhow::bail!("the system clock gave an unreadable date: {stamp}"),
    }
}

/// Fetch `url` and propose what its page states, read on `retrieved`.
fn propose(id: &str, url: &str, retrieved: AccessDate) -> anyhow::Result<Proposal> {
    use std::io::Read as _;
    let response = ureq::get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_PAGE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| anyhow::anyhow!("reading {url}: {e}"))?;
    let facts = extract(&String::from_utf8_lossy(&bytes));
    Ok(Proposal {
        id: id.to_owned(),
        url: url.to_owned(),
        retrieved,
        title: facts.title,
        published: facts.published,
        version: facts.version,
    })
}

/// What one page's metadata states.
struct PageFacts {
    title: Found<String>,
    published: Found<PublicationDate>,
    version: Found<String>,
}

/// One line saying what was found for a work and what was not.
fn summary(p: &Proposal) -> String {
    let show = |found: &Found<String>| match found {
        Found::Stated(v) => format!("{v:?}"),
        Found::NotStated => "not stated".to_owned(),
    };
    let published = match &p.published {
        Found::Stated(date) => format!("{date:?}"),
        Found::NotStated => "not stated".to_owned(),
    };
    format!(
        "{}: title {}, published {published}, version {} (read {}-{:02}-{:02})",
        p.id,
        show(&p.title),
        show(&p.version),
        p.retrieved.year,
        p.retrieved.month.number(),
        p.retrieved.day.get()
    )
}

/// What a page's own metadata states.
fn extract(page: &str) -> PageFacts {
    let metas = meta_tags(page);
    let meta = |names: &[&str]| {
        names.iter().find_map(|name| {
            metas
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, content)| content.trim().to_owned())
                .filter(|c| !c.is_empty())
        })
    };
    let title = meta(&["citation_title", "og:title", "dc.title"]).or_else(|| title_element(page));
    let published = meta(&[
        "citation_publication_date",
        "citation_date",
        "article:published_time",
        "dc.date.issued",
        "dcterms.issued",
        "datepublished",
    ])
    .and_then(|d| iso_date(&d));
    let version = meta(&["citation_version", "softwareversion"]);
    PageFacts {
        title: title.map_or(Found::NotStated, Found::Stated),
        published: published.map_or(Found::NotStated, Found::Stated),
        version: version.map_or(Found::NotStated, Found::Stated),
    }
}

/// Every `<meta>` tag's (`name` | `property` | `itemprop`, `content`) pair, in
/// page order.
fn meta_tags(page: &str) -> Vec<(String, String)> {
    let lower = page.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<meta").map(|i| at + i) {
        let Some(end) = lower[start..].find('>').map(|i| start + i) else {
            break;
        };
        let tag = &page[start..end];
        let key = attr(tag, "name")
            .or_else(|| attr(tag, "property"))
            .or_else(|| attr(tag, "itemprop"));
        if let (Some(key), Some(content)) = (key, attr(tag, "content")) {
            out.push((key, decode_entities(&content)));
        }
        at = end;
    }
    out
}

/// The value of attribute `name` in one tag's source, quoted either way.
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| from + i) {
        let before = lower[..i].chars().next_back();
        let rest = lower[i + name.len()..].trim_start();
        if before.is_some_and(char::is_whitespace) && rest.starts_with('=') {
            let value_start = tag.len() - rest.len() + 1;
            let value = tag[value_start..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                let body = &value[1..];
                return body.find(quote).map(|end| body[..end].to_owned());
            }
            return Some(
                value
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            );
        }
        from = i + name.len();
    }
    None
}

/// The text of the page's `<title>` element, whitespace collapsed.
fn title_element(page: &str) -> Option<String> {
    let lower = page.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    let text = decode_entities(&page[start..end])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

/// The five entities a title or attribute commonly carries. Anything else is
/// left as written rather than guessed at.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// An ISO 8601 date (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`, optionally followed by
/// a time), at the precision it states and no finer.
fn iso_date(text: &str) -> Option<PublicationDate> {
    let date = text.trim().split(['T', ' ']).next()?;
    let mut parts = date.split('-');
    let year = Year::new(parts.next()?.parse().ok()?)?;
    let Some(month) = parts.next() else {
        return Some(PublicationDate::Year(year));
    };
    let month = Month::from_number(month.parse().ok()?)?;
    let Some(day) = parts.next() else {
        return Some(PublicationDate::YearMonth { year, month });
    };
    let day = Day::new(day.parse().ok()?)?;
    let full = PublicationDate::Full { year, month, day };
    (parts.next().is_none() && full.names_a_day_that_exists()).then_some(full)
}

#[cfg(test)]
mod tests {
    use super::{extract, iso_date, propose, run};
    use rto_graph::{AccessDate, Day, Month, PublicationDate, Year};
    use rto_spec::Found;
    use std::io::{Read as _, Write as _};

    fn day(y: i32, m: Month, d: u8) -> AccessDate {
        AccessDate {
            year: Year::new(y).expect("year"),
            month: m,
            day: Day::new(d).expect("day"),
        }
    }

    /// A one-shot loopback server answering `body` — never the internet.
    fn stub(body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/spec", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\n\
                 last-modified: Tue, 01 Sep 2026 00:00:00 GMT\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write");
        });
        url
    }

    /// `refs fetch …` is the subcommand and `refs <key>` is still a key.
    #[test]
    fn fetch_parses_as_a_subcommand_beside_the_key() {
        use clap::Parser as _;
        let parse = |args: &[&str]| crate::Cli::try_parse_from(args).expect("parse").command;
        assert!(matches!(
            parse(&["roteiro", "refs", "fetch", "okf-spec", "--allow-download"]),
            crate::Command::Refs {
                action: Some(crate::RefsAction::Fetch {
                    allow_download: true,
                    ..
                }),
                ..
            }
        ));
        assert!(matches!(
            parse(&["roteiro", "refs", "site:okf-bundle"]),
            crate::Command::Refs {
                key: Some(_),
                action: None,
                ..
            }
        ));
    }

    #[test]
    fn without_consent_nothing_is_fetched() {
        // No id and no `--all` either, so even without the consent check this
        // could not reach the network: a removed check fails on the other message.
        let err = run(None, false, false).expect_err("refused");
        assert!(err.to_string().contains("--allow-download"), "{err}");
    }

    /// The page states a title and a date: both are proposed, the date at the
    /// precision stated, and the read date is an access date.
    #[test]
    fn a_stated_date_is_proposed_and_the_read_date_is_an_access_date() {
        let url = stub(
            "<html><head><title>Ignored &amp; overridden</title>\
             <meta name=\"citation_title\" content=\"Open Knowledge Format\">\
             <meta property='article:published_time' content='2026-06-12T09:00:00Z'>\
             </head><body></body></html>",
        );
        let read = day(2026, Month::November, 11);
        let p = propose("okf-spec", &url, read).expect("fetch");
        assert_eq!(p.retrieved, read);
        assert_eq!(p.title, Found::Stated("Open Knowledge Format".into()));
        assert_eq!(
            p.published,
            Found::Stated(PublicationDate::Full {
                year: Year::new(2026).expect("y"),
                month: Month::June,
                day: Day::new(12).expect("d"),
            })
        );
        assert_eq!(p.version, Found::NotStated);
    }

    /// No date on the page — even with a `Last-Modified` header and a year in
    /// the prose — is recorded as not stated, never guessed.
    #[test]
    fn a_page_with_no_stated_date_says_so() {
        let url = stub("<html><head><title>okf</title></head><body>© 2026</body></html>");
        let p = propose("okf-core", &url, day(2026, Month::November, 11)).expect("fetch");
        assert_eq!(p.title, Found::Stated("okf".into()));
        assert_eq!(p.published, Found::NotStated);
    }

    #[test]
    fn dates_keep_their_stated_precision_and_impossible_ones_are_refused() {
        assert_eq!(
            iso_date("2026"),
            Some(PublicationDate::Year(Year::new(2026).expect("y")))
        );
        assert!(matches!(
            iso_date("2026-06"),
            Some(PublicationDate::YearMonth { .. })
        ));
        assert_eq!(iso_date("2026-02-31"), None);
        assert_eq!(iso_date("June 2026"), None);
        assert_eq!(extract("<p>no head</p>").title, Found::NotStated);
    }
}

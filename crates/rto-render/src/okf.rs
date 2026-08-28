//! Render the graph as an **Open Knowledge Format** bundle (issue #663).
//!
//! OKF v0.2 is Google Cloud's vendor-neutral specification for the "LLM wiki"
//! pattern: a directory of markdown concept documents carrying YAML frontmatter,
//! reserved `index.md` and `log.md` files, and plain markdown links between
//! concepts. The whole specification fits on a page, and its only hard
//! requirement is that every concept document carries a non-empty `type`.
//!
//! <https://github.com/GoogleCloudPlatform/open-knowledge-format/blob/main/SPEC.md>
//!
//! # Why this replaced the Obsidian vault
//!
//! The vault was **one-way**: Roteiro wrote it, nothing read it back, and no
//! tool but Obsidian could consume it. An open format with named consumers earns
//! the same machinery better. Two concrete gains beyond that:
//!
//! - **The hierarchy retires a class of bug.** The vault flattened every note
//!   into one directory and appended a hash to each filename, because
//!   case-insensitive filesystems fold names that differ only in case — a defect
//!   that once cost this repository 104 notes of 8,144. OKF nests concepts in
//!   directories, so the collision the hash existed to survive does not arise.
//! - **Provenance stops being decoration.** Obsidian had nowhere to put it but a
//!   tag. OKF has a trust model, and it is the one Roteiro already computes.
//!
//! # The provenance mapping, which is the point
//!
//! Most producers will emit `type` and little else. Roteiro's authored/derived/
//! inferred distinction lands exactly on OKF's trust tiers (§5.3), which
//! consumers derive from `verified`:
//!
//! | [`Provenance`] | frontmatter | tier |
//! | --- | --- | --- |
//! | `Authored` — ADR and blueprint prose | `verified: [{ by: human:<id> }]` | human-reviewed |
//! | `Derived` — deterministic tree-sitter extraction | `verified: [{ by: roteiro/<version> }]` | machine-confirmed |
//! | `Inferred` — heuristic, carries a confidence | `generated:` alone | unverified |
//!
//! `Derived` is **machine-confirmed rather than unverified** on purpose: it is
//! reproduced deterministically from the AST at a known commit, so a consumer can
//! re-derive it. `Inferred` is a similarity judgement with a confidence score and
//! gets no `verified` key, because claiming otherwise would launder a guess into
//! a confirmation — the distinction the whole graph exists to keep.
//!
//! §7 makes the `human:` prefix load-bearing: it is the only thing that
//! separates human-reviewed from machine-confirmed, and producers **MUST** use it
//! for hand-authored content. Roteiro knows which nodes those are.
//!
//! # One deliberate divergence
//!
//! §11 says consumers **MUST NOT** reject a bundle for broken cross-links.
//! Roteiro treats a broken authored link as drift and fails a gate over it. Both
//! are right — the specification asks consumers to be liberal; Roteiro is a
//! producer that guarantees more than it must. A Roteiro bundle should not
//! contain a broken link, and `roteiro check` is the reason.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rto_graph::{Explanation, NodeSummary, Provenance};

/// The specification version this renderer targets, written into the bundle
/// root's `index.md` as `okf_version` (§10 — the one place frontmatter is
/// permitted in an index).
pub const OKF_VERSION: &str = "0.2";

/// The reserved filename for a directory listing (§8).
pub const INDEX_FILE: &str = "index.md";

/// The reserved filename for a change log (§9).
pub const LOG_FILE: &str = "log.md";

/// One rendered file in the bundle: a bundle-relative path and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFile {
    /// Path relative to the bundle root, always `/`-separated.
    pub path: String,
    /// The file's full text, including any frontmatter block.
    pub content: String,
}

/// Who produced or confirmed a concept, in the actor form §7 requires.
///
/// The three shapes are not interchangeable: a consumer classifying trust keys
/// off the `human:` prefix, so using the wrong one silently moves a concept
/// between tiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A person: `human:<id>`. The only form that yields the human-reviewed tier.
    Human(String),
    /// A tool, as `<producer>/<version>`.
    Tool(String, String),
    /// An automated process: `process:<id>`.
    Process(String),
}

impl Actor {
    /// The wire form, exactly as §7 specifies it.
    #[must_use]
    pub fn as_token(&self) -> String {
        match self {
            Self::Human(id) => format!("human:{id}"),
            Self::Tool(producer, version) => format!("{producer}/{version}"),
            Self::Process(id) => format!("process:{id}"),
        }
    }
}

/// How a concept came to exist, rendered into `generated` / `verified`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The actor that produced the concept.
    pub by: Actor,
    /// When, as an ISO 8601 instant.
    pub at: String,
    /// Whether this origin also *confirms* the concept.
    ///
    /// `Authored` and `Derived` do; `Inferred` does not. See the module doc — a
    /// heuristic that claimed confirmation would launder a guess.
    pub confirms: bool,
}

/// A concept document's frontmatter.
///
/// Only [`Self::type_`] is required by the specification; every other field is
/// omitted entirely when absent rather than written empty, because §11 tells
/// consumers not to reject a document for a missing optional field and an empty
/// string is a different claim from silence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    /// `type` — the one required key. Named with a trailing underscore because
    /// `type` is a Rust keyword; it is written as `type`.
    pub type_: String,
    /// `title` — human-readable display name.
    pub title: Option<String>,
    /// `description` — a single-sentence summary.
    pub description: Option<String>,
    /// `resource` — canonical URI for the underlying asset.
    pub resource: Option<String>,
    /// `tags` — categorisation strings.
    pub tags: Vec<String>,
    /// `status` — `draft` | `stable` | `deprecated`.
    pub status: Option<String>,
    /// The origin, split into `generated` and `verified` on render.
    pub origin: Option<Origin>,
    /// `sources` — where the concept derives from, each with a `resource`.
    pub sources: Vec<String>,
}

/// Quote a scalar for YAML, always.
///
/// Unconditional rather than clever: a value that looks like a number, a date,
/// `yes`, `no`, `null` or `~` changes type under a YAML parser when written
/// bare, and a concept `type` of `no` becoming the boolean `false` is exactly
/// the failure that makes a bundle non-conformant while looking fine.
fn yaml_scalar(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

impl Frontmatter {
    /// Render the frontmatter block, `---` fences included.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from("---\n");
        let _ = writeln!(out, "type: {}", yaml_scalar(&self.type_));
        for (key, value) in [
            ("title", self.title.as_deref()),
            ("description", self.description.as_deref()),
            ("resource", self.resource.as_deref()),
            ("status", self.status.as_deref()),
        ] {
            if let Some(v) = value {
                let _ = writeln!(out, "{key}: {}", yaml_scalar(v));
            }
        }
        if !self.tags.is_empty() {
            out.push_str("tags:\n");
            for t in &self.tags {
                let _ = writeln!(out, "  - {}", yaml_scalar(t));
            }
        }
        if let Some(origin) = &self.origin {
            // `generated` always: it records production, which happened whether or
            // not anyone confirmed the result.
            let _ = writeln!(
                out,
                "generated:\n  by: {}\n  at: {}",
                yaml_scalar(&origin.by.as_token()),
                yaml_scalar(&origin.at)
            );
            // `verified` only when the origin confirms. Its **absence** is the
            // unverified tier, so writing an empty list here would claim a
            // confirmation nobody made.
            if origin.confirms {
                let _ = writeln!(
                    out,
                    "verified:\n  - by: {}\n    at: {}",
                    yaml_scalar(&origin.by.as_token()),
                    yaml_scalar(&origin.at)
                );
            }
        }
        if !self.sources.is_empty() {
            out.push_str("sources:\n");
            for s in &self.sources {
                let _ = writeln!(out, "  - resource: {}", yaml_scalar(s));
            }
        }
        out.push_str("---\n");
        out
    }
}

/// The bundle directory a node kind belongs in.
///
/// Grouping by kind is what gives the bundle its hierarchy, and with it a
/// meaningful per-directory `index.md`. Code symbols share one directory rather
/// than splitting `fn` from `struct`, because a reader looking for a symbol does
/// not know which it is.
#[must_use]
pub fn section_for(kind: &str) -> &'static str {
    match kind {
        "adr" | "adr_section" => "decisions",
        "blueprint" => "blueprints",
        "doc" => "docs",
        "file" => "files",
        "marker" => "debt",
        _ => "symbols",
    }
}

/// Slug a node key into a filename that is safe on every filesystem and stable
/// across renders.
///
/// Unlike the vault this replaces, the result does **not** need a hash appended:
/// concepts live in per-kind directories, so the cross-kind collisions the vault
/// hashed around cannot occur here. Two keys that still slug identically within
/// one directory are disambiguated by the caller, which can see the whole set.
#[must_use]
pub fn slug(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut last_dash = false;
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_owned();
    if trimmed.is_empty() {
        "concept".to_owned()
    } else {
        trimmed
    }
}

/// The bundle-relative path a node is written to, always beginning with `/` so
/// it can be used as a link target verbatim (§6 — absolute, bundle-relative).
#[must_use]
pub fn concept_path(node: &NodeSummary) -> String {
    format!("/{}/{}.md", section_for(&node.kind), slug(&node.key))
}

/// Map a graph provenance onto an OKF origin.
///
/// See the module documentation for why `Derived` confirms and `Inferred` does
/// not. `tool` is the producing tool's actor, used for everything a machine
/// produced; `human` is the authored content's confirmer, which the caller
/// resolves from the commit that introduced it.
#[must_use]
pub fn origin_for(prov: Provenance, at: &str, tool: &Actor, human: Option<&Actor>) -> Origin {
    match prov {
        // Authored prose is confirmed by the person who wrote it. Falling back to
        // the tool when the author is unknown would move the concept from
        // human-reviewed to machine-confirmed, so an unknown author yields no
        // confirmation at all rather than the wrong one.
        Provenance::Authored => match human {
            Some(actor) => Origin {
                by: actor.clone(),
                at: at.to_owned(),
                confirms: true,
            },
            None => Origin {
                by: tool.clone(),
                at: at.to_owned(),
                confirms: false,
            },
        },
        // Deterministic extraction: a consumer can re-derive it from the same
        // commit and get the same answer, which is what machine-confirmed means.
        Provenance::Derived => Origin {
            by: tool.clone(),
            at: at.to_owned(),
            confirms: true,
        },
        // A similarity judgement carrying a confidence. Unverified, and honestly so.
        Provenance::Inferred => Origin {
            by: tool.clone(),
            at: at.to_owned(),
            confirms: false,
        },
    }
}

/// Render one node as an OKF concept document.
///
/// `body` is the node's prose when it has any. Relationships become plain
/// markdown links under a heading, which is how §6 says a relationship is
/// asserted — the link carries the relationship, and the surrounding prose says
/// what kind it is.
#[must_use]
pub fn render_concept(
    ex: &Explanation,
    fm: &Frontmatter,
    body: Option<&str>,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> BundleFile {
    let mut content = fm.render();
    content.push('\n');
    let _ = writeln!(
        content,
        "# {}\n",
        fm.title.as_deref().unwrap_or(&ex.node.name)
    );
    if let Some(text) = body.map(str::trim).filter(|t| !t.is_empty()) {
        content.push_str(text);
        content.push_str("\n\n");
    }

    // Group by edge kind so the prose above each list can name the relationship.
    let mut groups: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (edge, direction) in ex
        .outgoing
        .iter()
        .map(|e| (e, "→"))
        .chain(ex.incoming.iter().map(|e| (e, "←")))
    {
        if let Some(target) = resolve(&edge.node) {
            let label = edge.node.rsplit(':').next().unwrap_or(&edge.node);
            let confidence = edge
                .confidence
                .map(|c| format!(" (confidence {c:.2})"))
                .unwrap_or_default();
            groups
                .entry(edge.kind.as_str())
                .or_default()
                .push(format!("* {direction} [{label}]({target}){confidence}"));
        }
    }
    if !groups.is_empty() {
        content.push_str("## Relationships\n\n");
        for (kind, mut links) in groups {
            links.sort();
            links.dedup();
            let _ = writeln!(content, "### {kind}\n");
            for link in links {
                let _ = writeln!(content, "{link}");
            }
            content.push('\n');
        }
    }

    BundleFile {
        path: concept_path(&ex.node),
        content,
    }
}

/// One entry in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Display title.
    pub title: String,
    /// Link target, bundle-relative.
    pub target: String,
    /// Short description, taken from the concept's own frontmatter (§8 SHOULD).
    pub description: Option<String>,
}

/// Render a directory `index.md` (§8).
///
/// Deliberately **no frontmatter**: §8 permits it only in the bundle root, and a
/// stray block in a nested index would make the file a malformed concept rather
/// than a valid listing.
#[must_use]
pub fn render_index(heading: &str, entries: &[IndexEntry]) -> String {
    let mut out = format!("# {heading}\n\n");
    for e in entries {
        let desc = e
            .description
            .as_deref()
            .map(|d| format!(" - {d}"))
            .unwrap_or_default();
        let _ = writeln!(out, "* [{}]({}){desc}", e.title, e.target);
    }
    out
}

/// Render the bundle-root `index.md`, the one index that carries frontmatter.
#[must_use]
pub fn render_root_index(heading: &str, entries: &[IndexEntry]) -> String {
    let mut out = format!("---\nokf_version: {}\n---\n\n", yaml_scalar(OKF_VERSION));
    out.push_str(&render_index(heading, entries));
    out
}

/// One dated group of log entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogDay {
    /// ISO 8601 `YYYY-MM-DD`. §9 requires this exact form for date headings.
    pub date: String,
    /// The day's entries, each already prefixed with its kind (`**Update**: …`).
    pub entries: Vec<String>,
}

/// Render `log.md` (§9): dated groups, newest first.
#[must_use]
pub fn render_log(heading: &str, days: &[LogDay]) -> String {
    let mut out = format!("# {heading}\n\n");
    for day in days {
        let _ = writeln!(out, "## {}\n", day.date);
        for entry in &day.entries {
            let _ = writeln!(out, "* {entry}");
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> Actor {
        Actor::Tool("roteiro".into(), "4.0.0".into())
    }

    #[test]
    fn the_only_required_field_is_type() {
        let fm = Frontmatter {
            type_: "adr".into(),
            ..Frontmatter::default()
        };
        let rendered = fm.render();
        assert_eq!(rendered, "---\ntype: \"adr\"\n---\n");
    }

    #[test]
    fn actors_use_the_forms_the_spec_requires() {
        assert_eq!(Actor::Human("pixie79".into()).as_token(), "human:pixie79");
        assert_eq!(tool().as_token(), "roteiro/4.0.0");
        assert_eq!(
            Actor::Process("nightly".into()).as_token(),
            "process:nightly"
        );
    }

    /// The trust tiers of §5.3, asserted through the rendered frontmatter rather
    /// than through `Origin`, because the tier is what a consumer derives.
    #[test]
    fn provenance_maps_onto_the_trust_tiers() {
        let human = Actor::Human("pixie79".into());
        let at = "2026-08-28T10:00:00Z";

        let authored = origin_for(Provenance::Authored, at, &tool(), Some(&human));
        let fm = Frontmatter {
            type_: "adr".into(),
            origin: Some(authored),
            ..Frontmatter::default()
        };
        let rendered = fm.render();
        assert!(
            rendered.contains("verified:") && rendered.contains("human:pixie79"),
            "authored prose is human-reviewed: {rendered}"
        );

        let derived = origin_for(Provenance::Derived, at, &tool(), Some(&human));
        let fm = Frontmatter {
            type_: "fn".into(),
            origin: Some(derived),
            ..Frontmatter::default()
        };
        let rendered = fm.render();
        assert!(
            rendered.contains("verified:"),
            "deterministic extraction is machine-confirmed: {rendered}"
        );
        assert!(
            !rendered.contains("human:"),
            "but it is not human-reviewed — the prefix is the only thing that \
             separates the tiers: {rendered}"
        );

        let inferred = origin_for(Provenance::Inferred, at, &tool(), Some(&human));
        let fm = Frontmatter {
            type_: "fn".into(),
            origin: Some(inferred),
            ..Frontmatter::default()
        };
        let rendered = fm.render();
        assert!(
            rendered.contains("generated:"),
            "a heuristic still records that it was produced: {rendered}"
        );
        assert!(
            !rendered.contains("verified:"),
            "but claims no confirmation — absence *is* the unverified tier, so an \
             empty list here would launder a guess: {rendered}"
        );
    }

    /// An authored node whose author is unknown must not silently become
    /// machine-confirmed.
    #[test]
    fn an_authored_node_with_no_known_human_claims_nothing() {
        let o = origin_for(Provenance::Authored, "2026-08-28T10:00:00Z", &tool(), None);
        assert!(
            !o.confirms,
            "falling back to the tool would move the concept between trust tiers"
        );
    }

    #[test]
    fn scalars_are_quoted_so_yaml_cannot_retype_them() {
        // `no`, `12:30` and `1.0` all change type when written bare.
        for raw in ["no", "yes", "null", "~", "12:30", "1.0", "on"] {
            let fm = Frontmatter {
                type_: raw.into(),
                ..Frontmatter::default()
            };
            assert_eq!(fm.render(), format!("---\ntype: \"{raw}\"\n---\n"));
        }
    }

    #[test]
    fn a_nested_index_carries_no_frontmatter_but_the_root_does() {
        let entries = [IndexEntry {
            title: "ADR-0001".into(),
            target: "/decisions/adr-0001.md".into(),
            description: Some("The founding decision.".into()),
        }];
        let nested = render_index("Decisions", &entries);
        assert!(
            !nested.starts_with("---"),
            "§8 permits frontmatter only in the bundle root: {nested}"
        );
        assert!(nested.contains("* [ADR-0001](/decisions/adr-0001.md) - The founding decision."));

        let root = render_root_index("Bundle", &entries);
        assert!(
            root.starts_with("---\nokf_version: \"0.2\"\n---\n"),
            "{root}"
        );
    }

    #[test]
    fn log_days_use_iso_8601_headings() {
        let log = render_log(
            "Update Log",
            &[LogDay {
                date: "2026-08-28".into(),
                entries: vec!["**Update**: rebuilt from `74fad8f`.".into()],
            }],
        );
        assert!(log.contains("## 2026-08-28\n"), "{log}");
        assert!(
            log.contains("* **Update**: rebuilt from `74fad8f`."),
            "{log}"
        );
    }

    #[test]
    fn concepts_are_grouped_into_per_kind_directories() {
        assert_eq!(section_for("adr"), "decisions");
        assert_eq!(section_for("adr_section"), "decisions");
        assert_eq!(section_for("blueprint"), "blueprints");
        assert_eq!(section_for("file"), "files");
        assert_eq!(section_for("marker"), "debt");
        // Every code symbol shares one directory: a reader looking for `greet`
        // does not know whether it is a fn, a struct or a trait.
        assert_eq!(section_for("fn"), "symbols");
        assert_eq!(section_for("struct"), "symbols");
        assert_eq!(section_for("trait"), "symbols");
    }

    #[test]
    fn slugs_are_stable_and_filesystem_safe() {
        assert_eq!(
            slug("sym:rust:src/main.rs#greet"),
            "sym-rust-src-main-rs-greet"
        );
        assert_eq!(slug("adr:0001#decision"), "adr-0001-decision");
        // No trailing separator, no empty result, no run of dashes.
        assert_eq!(slug("a//b"), "a-b");
        assert_eq!(slug("trailing///"), "trailing");
        assert_eq!(slug("###"), "concept");
    }
}

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
//! for hand-authored content. Roteiro knows which nodes those are, and resolves
//! *which person* per document — the author of the commit that last changed that
//! document's path. Naming one author for the whole repository would record a
//! review that person never did, on every ADR at once.
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
///
/// # Deliberately exhaustive
///
/// This is deliberately not `#[non_exhaustive]`, though these crates are
/// published and a fourth variant would therefore be a breaking change. **The
/// set is closed by the specification, not by us**: §7 defines exactly these
/// three forms, and a
/// fourth appearing means OKF changed. When that happens a caller matching on
/// this enum *should* stop compiling, because a new actor form is a decision
/// about trust that must be looked at rather than absorbed by a wildcard arm.
///
/// `#[non_exhaustive]` would buy version-compatibility at the price of making
/// that change silent — which is the opposite of what the trust model needs.
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

/// The longest slug a filename may carry, before any disambiguating suffix.
///
/// `NAME_MAX` is 255 bytes on Linux and macOS. Real keys reach it: rendering this
/// repository failed with `File name too long (os error 63)` on a symbol key,
/// **after** writing part of the bundle — a unit test over short fixtures could
/// not have found it, and did not. The headroom below covers the `-` plus an
/// eight-character digest plus `.md`.
const MAX_SLUG: usize = 200;

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
        return "concept".to_owned();
    }
    if trimmed.len() <= MAX_SLUG {
        return trimmed;
    }
    // Truncation can *create* a collision that the full keys did not have — two
    // long keys sharing a prefix become one name — so a shortened slug always
    // carries a digest of the whole key. Cutting on a char boundary is free here
    // because every retained character is ASCII.
    let keep = MAX_SLUG - 9;
    format!("{}-{}", &trimmed[..keep], short_digest(key))
}

/// The bundle-relative path a node takes in a single-project bundle whose slug
/// did not collide, always beginning with `/` so it can be used as a link target
/// verbatim (§6 — absolute, bundle-relative).
///
/// **Provisional, not authoritative.** [`assemble`] overwrites it, because the
/// real path also carries the workspace member's directory and a disambiguating
/// digest when two keys slug alike — neither of which is visible from one node.
/// Resolving a *link* with this function is the bug it exists to make obvious:
/// use the placement [`assemble`] passes to [`render_concept`].
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
    let text = body.map(str::trim).filter(|t| !t.is_empty());
    // A document that opens with its own `#` heading keeps it. Writing the title
    // above it would give the concept two H1s saying nearly the same thing, and
    // the document's own is the better one — it is what its author wrote.
    let body_leads_with_heading = text.is_some_and(|t| t.starts_with("# "));
    if !body_leads_with_heading {
        let _ = writeln!(
            content,
            "# {}\n",
            fm.title.as_deref().unwrap_or(&ex.node.name)
        );
    }
    if let Some(text) = text {
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

/// A concept ready to be written: its node, its frontmatter, and its prose.
pub struct Concept<'a> {
    /// The graph node and its neighbourhood.
    pub explanation: &'a Explanation,
    /// The frontmatter to render.
    pub frontmatter: Frontmatter,
    /// The node's prose body, when it has one.
    pub body: Option<String>,
    /// The workspace member this concept came from, for a bundle spanning several
    /// repositories (ADR-0009). `None` for a single project.
    ///
    /// Nesting by member is what stops two repositories' `file:README.md` landing
    /// on one path. The vault this replaces solved the same problem by qualifying
    /// the *key* and hashing the filename, because it had one flat directory to
    /// work with; a bundle has directories, so the structure carries it.
    pub member: Option<String>,
}

/// One directory's concepts, each with the path [`assemble`]'s first pass gave
/// it — the intermediate the second pass renders from.
struct Placed<'a> {
    /// The workspace member these concepts came from, when the bundle spans one.
    /// Also the scope a link resolves in: the same key in two members is two
    /// concepts.
    member: Option<String>,
    /// The bundle-relative directory: `<member>/<section>`, or `<section>` alone.
    dir: String,
    /// Each concept and the bundle-relative path it will be written to.
    concepts: Vec<(Concept<'a>, String)>,
}

/// Assemble a whole bundle: every concept, a per-directory `index.md`, and the
/// bundle-root `index.md` carrying `okf_version`.
///
/// # Collisions are resolved here, and only here
///
/// [`slug`] can map two different keys onto one filename. The Obsidian vault this
/// replaces appended a hash to **every** note to survive that, because it wrote
/// one flat directory on filesystems that fold case — and it still lost 104 notes
/// of 8,144 before the hash existed. Nesting by kind removes most of the pressure,
/// but not all of it, so the remaining collisions are settled where the whole set
/// is visible rather than by a per-name rule that cannot see its neighbours.
///
/// A colliding name gets a short digest of its key appended. The **first** name in
/// key order keeps the bare slug, so a bundle re-rendered from an unchanged graph
/// is byte-identical: the disambiguation depends on the set, and the set is sorted.
///
/// Comparison is case-**insensitive** on purpose. `Foo` and `foo` are one file on
/// macOS and Windows, and a bundle that wrote both would silently lose one — which
/// is exactly how the vault lost notes.
///
/// # Links are resolved against the placement, not re-derived from the key
///
/// Which is why this happens in two passes. A concept's path depends on the whole
/// set — the member directory it nests under, and whether its slug collided — so
/// *any* rule that turns a key into a path on its own is guessing. The first pass
/// places every concept and records `key -> path`; the second renders, resolving
/// each relationship through that map. A key the map does not hold is not in the
/// bundle, and its link is dropped rather than written as a path that does not
/// exist.
///
/// The map is scoped **per member**: `file:README.md` is a different concept in
/// each repository of a workspace, so a link from one member's concept resolves
/// inside that member.
#[must_use]
pub fn assemble(concepts: Vec<Concept<'_>>, title: &str, log: &[LogDay]) -> Vec<BundleFile> {
    // Group by section, in key order, so both the output and the disambiguation
    // are deterministic.
    let mut by_section: BTreeMap<(Option<String>, &'static str), Vec<Concept<'_>>> =
        BTreeMap::new();
    let mut ordered = concepts;
    ordered.sort_by(|a, b| a.explanation.node.key.cmp(&b.explanation.node.key));
    for c in ordered {
        by_section
            .entry((c.member.clone(), section_for(&c.explanation.node.kind)))
            .or_default()
            .push(c);
    }

    // Pass one: place every concept. Nothing is rendered yet, because a link
    // written now could only guess at a path this pass is still deciding.
    let mut placed: Vec<Placed<'_>> = Vec::new();
    let mut index: BTreeMap<Option<String>, BTreeMap<String, String>> = BTreeMap::new();

    for ((member, section), members) in by_section {
        // `/<member>/<section>/` in a workspace, `/<section>/` on its own.
        let dir = member
            .as_deref()
            .map_or_else(|| section.to_owned(), |m| format!("{}/{section}", slug(m)));
        let mut taken: BTreeMap<String, usize> = BTreeMap::new();
        let mut concepts: Vec<(Concept<'_>, String)> = Vec::with_capacity(members.len());
        let member_index = index.entry(member.clone()).or_default();

        for c in members {
            // Case folding is already handled: `slug` lowercases, so no two slugs
            // can differ by case alone and this comparison needs no folding of its
            // own. An earlier version folded again here and read as the guard
            // against case-insensitive filesystems — it was a no-op, and removing
            // it changed no test, which is how the redundancy was found.
            let base = slug(&c.explanation.node.key);
            let name = match taken.get(&base) {
                None => base.clone(),
                Some(_) => format!("{base}-{}", short_digest(&c.explanation.node.key)),
            };
            *taken.entry(base).or_insert(0) += 1;

            let path = format!("/{dir}/{name}.md");
            member_index.insert(c.explanation.node.key.clone(), path.clone());
            concepts.push((c, path));
        }
        placed.push(Placed {
            member,
            dir,
            concepts,
        });
    }

    let mut files = Vec::new();
    let mut sections: Vec<IndexEntry> = Vec::new();

    // Pass two: render, resolving every link through the placement above.
    for section in placed {
        let member_index = index.get(&section.member);
        let dir = &section.dir;
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(section.concepts.len());

        for (c, path) in &section.concepts {
            let title = c
                .frontmatter
                .title
                .clone()
                .unwrap_or_else(|| c.explanation.node.name.clone());
            entries.push(IndexEntry {
                title,
                target: path.clone(),
                description: c.frontmatter.description.clone(),
            });
            let mut file =
                render_concept(c.explanation, &c.frontmatter, c.body.as_deref(), &|key| {
                    member_index.and_then(|m| m.get(key)).cloned()
                });
            file.path.clone_from(path);
            files.push(file);
        }

        files.push(BundleFile {
            path: format!("/{dir}/{INDEX_FILE}"),
            content: render_index(dir, &entries),
        });
        sections.push(IndexEntry {
            title: dir.clone(),
            target: format!("/{dir}/{INDEX_FILE}"),
            description: Some(format!("{} concept(s)", section.concepts.len())),
        });
    }

    if !log.is_empty() {
        files.push(BundleFile {
            path: format!("/{LOG_FILE}"),
            content: render_log("Update Log", log),
        });
    }
    files.push(BundleFile {
        path: format!("/{INDEX_FILE}"),
        content: render_root_index(title, &sections),
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// A short, stable digest of a key, for disambiguating a collided slug.
///
/// FNV-1a rather than a cryptographic hash: this is a filename disambiguator, not
/// a security boundary, and it must stay identical across renders and platforms.
fn short_digest(key: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:08x}")[..8].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, kind: &str, name: &str) -> NodeSummary {
        NodeSummary {
            key: key.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            path: None,
            lang: None,
        }
    }

    fn explanation(key: &str, kind: &str, name: &str) -> Explanation {
        Explanation {
            schema: rto_graph::SCHEMA,
            node: node(key, kind, name),
            meta: serde_json::Value::Null,
            outgoing: Vec::new(),
            incoming: Vec::new(),
        }
    }

    fn concept<'a>(ex: &'a Explanation, type_: &str) -> Concept<'a> {
        Concept {
            explanation: ex,
            frontmatter: Frontmatter {
                type_: type_.to_owned(),
                ..Frontmatter::default()
            },
            body: None,
            member: None,
        }
    }

    fn edge(to: &str) -> rto_graph::EdgeRef {
        rto_graph::EdgeRef {
            kind: "references".to_owned(),
            provenance: "authored",
            confidence: None,
            node: to.to_owned(),
        }
    }

    /// Every `](/…)` link in an emitted bundle, as `(containing file, target)`.
    fn internal_links(files: &[BundleFile]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for f in files {
            let mut rest = f.content.as_str();
            while let Some(open) = rest.find("](/") {
                rest = &rest[open + 2..];
                let Some(close) = rest.find(')') else { break };
                out.push((f.path.clone(), rest[..close].to_owned()));
                rest = &rest[close..];
            }
        }
        out
    }

    /// **Every internal link points at a file the bundle actually contains.**
    ///
    /// The conformance test above cannot make this assertion, and would not have
    /// caught its failure: §11 tells consumers they **MUST NOT** reject a bundle
    /// for a broken cross-link, so a bundle full of them is still conformant. It
    /// is still wrong, and this repository promises better (ADR-0021).
    ///
    /// Three ways a link target can differ from a key's own slug, all present in
    /// the fixture because a resolver that re-derives the path from the key gets
    /// each of them wrong:
    ///
    /// 1. a **workspace member** prefixes the directory;
    /// 2. a **collided slug** takes a digest suffix;
    /// 3. a node whose **kind and key disagree** about the section —
    ///    `blueprint_section` keys begin `blueprint:` but the concept files under
    ///    `symbols`, which is how 43 links broke in a real render of this
    ///    repository.
    #[test]
    fn every_emitted_link_resolves_to_a_file_that_exists() {
        // (3) key says `blueprint:`, kind says `blueprint_section` → `symbols`.
        let section = {
            let mut ex = explanation(
                "blueprint:docs/blueprint/roteiro.md#1-crate-placement",
                "blueprint_section",
                "1 · Crate placement",
            );
            ex.outgoing = vec![edge("blueprint:docs/blueprint/roteiro.md")];
            ex
        };
        let plan = {
            let mut ex = explanation(
                "blueprint:docs/blueprint/roteiro.md",
                "blueprint",
                "roteiro.md",
            );
            // (2) both collision partners, and the section above.
            ex.outgoing = vec![
                edge("blueprint:docs/blueprint/roteiro.md#1-crate-placement"),
                edge("sym:rust:a/b.rs#Thing"),
                edge("sym:rust:a-b.rs#thing"),
            ];
            ex
        };
        let thing_a = explanation("sym:rust:a/b.rs#Thing", "fn", "Thing");
        let thing_b = explanation("sym:rust:a-b.rs#thing", "fn", "thing");
        assert_eq!(
            slug(&thing_a.node.key),
            slug(&thing_b.node.key),
            "the fixture must actually collide, or the digest suffix is never exercised"
        );

        // (1) everything nests under one workspace member.
        let concepts: Vec<Concept<'_>> = [
            (&section, "blueprint_section"),
            (&plan, "blueprint"),
            (&thing_a, "fn"),
            (&thing_b, "fn"),
        ]
        .into_iter()
        .map(|(ex, type_)| {
            let mut c = concept(ex, type_);
            c.member = Some("Alpha".to_owned());
            c
        })
        .collect();

        let files = assemble(concepts, "Workspace", &[]);
        let emitted: std::collections::BTreeSet<&str> =
            files.iter().map(|f| f.path.as_str()).collect();

        // The fixture is load-bearing only if the placement really did all three
        // things. Asserted before the links, so a fixture that stopped exercising
        // one of them fails here rather than passing vacuously below.
        assert!(
            emitted
                .iter()
                .all(|p| *p == "/index.md" || p.starts_with("/alpha/")),
            "every concept must nest under its member: {emitted:?}"
        );
        assert!(
            emitted.contains("/alpha/symbols/sym-rust-a-b-rs-thing.md"),
            "the first collision partner keeps the bare slug: {emitted:?}"
        );
        assert!(
            emitted
                .iter()
                .any(|p| p.starts_with("/alpha/symbols/sym-rust-a-b-rs-thing-")),
            "the second takes a digest suffix: {emitted:?}"
        );
        assert!(
            emitted.contains(
                "/alpha/symbols/blueprint-docs-blueprint-roteiro-md-1-crate-placement.md"
            ),
            "a `blueprint_section` files under `symbols`, not under its key's \
             `blueprints`: {emitted:?}"
        );

        let links = internal_links(&files);
        // A resolver that drops what it cannot place satisfies the loop below by
        // emitting nothing, so count first: 4 relationship links (one per edge),
        // 4 concept entries across the two directory indexes, and 2 directory
        // entries in the root index.
        assert_eq!(links.len(), 4 + 4 + 2, "{links:?}");

        for (from, target) in &links {
            assert!(
                emitted.contains(target.as_str()),
                "{from} links to {target}, which the bundle does not contain: {emitted:?}"
            );
        }

        // Existence is not enough, and this is the half that is easy to miss: two
        // concepts whose slugs collided are *different files*, so a resolver that
        // re-derives the bare slug sends both links to whichever one kept it. That
        // target exists, so the loop above passes while the link points at the
        // wrong concept — silently wrong rather than broken. `plan` has three
        // distinct edge targets and must therefore emit three distinct paths.
        let plan_path = "/alpha/blueprints/blueprint-docs-blueprint-roteiro-md.md";
        let from_plan: std::collections::BTreeSet<&str> = links
            .iter()
            .filter(|(from, _)| from == plan_path)
            .map(|(_, target)| target.as_str())
            .collect();
        assert_eq!(
            from_plan.len(),
            plan.outgoing.len(),
            "{plan_path} has {} edges to distinct concepts but links to {} file(s): {from_plan:?}",
            plan.outgoing.len(),
            from_plan.len()
        );
    }

    /// Every file the bundle emits satisfies §11's conformance criteria.
    ///
    /// Asserted over the *emitted set* rather than over the renderer, because the
    /// specification is a statement about a bundle and a per-function test cannot
    /// make it.
    #[test]
    fn every_emitted_bundle_is_conformant() {
        let a = explanation("adr:0001#decision", "adr", "ADR-0001");
        let b = explanation("sym:rust:src/main.rs#greet", "fn", "greet");
        let files = assemble(
            vec![concept(&a, "adr"), concept(&b, "fn")],
            "Roteiro",
            &[LogDay {
                date: "2026-08-28".into(),
                entries: vec!["**Update**: rebuilt.".into()],
            }],
        );

        for f in &files {
            let reserved = f.path.ends_with(INDEX_FILE) || f.path.ends_with(LOG_FILE);
            if reserved {
                continue;
            }
            // §11.1 — a parseable frontmatter block, and §11.2 a non-empty `type`.
            assert!(
                f.content.starts_with("---\n"),
                "{} opens with no frontmatter block",
                f.path
            );
            let end = f.content[4..]
                .find("\n---\n")
                .expect("frontmatter must terminate");
            let block = &f.content[4..4 + end];
            assert!(
                block
                    .lines()
                    .any(|l| l.starts_with("type: ") && l.len() > 8),
                "{} carries no non-empty `type`: {block}",
                f.path
            );
        }

        // §8 — a nested index carries no frontmatter; only the root may.
        let nested = files
            .iter()
            .find(|f| f.path == "/decisions/index.md")
            .expect("a per-directory index");
        assert!(!nested.content.starts_with("---"), "{}", nested.content);
        let root = files
            .iter()
            .find(|f| f.path == "/index.md")
            .expect("a root index");
        assert!(
            root.content.contains("okf_version: \"0.2\""),
            "{}",
            root.content
        );
    }

    /// A document that brings its own heading is not given a second one.
    #[test]
    fn a_body_with_its_own_heading_is_not_double_titled() {
        let ex = explanation("adr:0010", "adr", "ADR-0010");
        let fm = Frontmatter {
            type_: "adr".into(),
            title: Some("Explorer web app".into()),
            ..Frontmatter::default()
        };
        let with = render_concept(
            &ex,
            &fm,
            Some("# ADR-0010: Explorer web app\n\nBody."),
            &|_| None,
        );
        let h1s = |c: &str| c.lines().filter(|l| l.starts_with("# ")).count();
        assert_eq!(h1s(&with.content), 1, "exactly one H1: {}", with.content);
        assert!(with.content.contains("# ADR-0010: Explorer web app"));
        assert!(
            !with.content.contains("# Explorer web app\n\n# ADR-0010"),
            "the frontmatter title must not be stacked above the document's own"
        );

        // A body with no heading still gets one, or the concept has no title at all.
        let without = render_concept(&ex, &fm, Some("Just prose."), &|_| None);
        assert!(
            without.content.contains("# Explorer web app"),
            "a headingless body still gets the title: {}",
            without.content
        );
        assert_eq!(h1s(&without.content), 1);
    }

    /// Two members' identically-named concepts do not collide.
    ///
    /// Every repository has a `README.md`, so `file:README.md` is the same key in
    /// each — the case the vault this replaces had to qualify keys and hash
    /// filenames to survive, because it wrote one flat directory. Nesting by
    /// member carries it structurally instead, and the assertion is again the one
    /// whose failure was invisible: **both concepts are written**.
    #[test]
    fn two_members_sharing_a_key_both_survive() {
        let a = explanation("file:README.md", "file", "README.md");
        let b = explanation("file:README.md", "file", "README.md");
        let mut ca = concept(&a, "file");
        ca.member = Some("app".to_owned());
        let mut cb = concept(&b, "file");
        cb.member = Some("lib".to_owned());

        let files = assemble(vec![ca, cb], "Workspace", &[]);
        let concepts: Vec<&BundleFile> = files
            .iter()
            .filter(|f| !f.path.ends_with(INDEX_FILE) && !f.path.ends_with(LOG_FILE))
            .collect();
        assert_eq!(concepts.len(), 2, "both members' README must be written");
        assert!(
            concepts.iter().any(|f| f.path.starts_with("/app/")),
            "one under its member: {:?}",
            concepts.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert!(concepts.iter().any(|f| f.path.starts_with("/lib/")));
    }

    /// A key longer than the filesystem allows is truncated, and truncation does
    /// not merge two concepts into one.
    ///
    /// Found by *running* the renderer over this repository, not by a unit test:
    /// it failed with `File name too long (os error 63)` after writing part of
    /// the bundle. Short fixtures cannot reach this, which is why the earlier
    /// tests were all green while the real render was broken.
    #[test]
    fn an_overlong_key_is_truncated_without_colliding() {
        let long = "sym:rust:".to_owned() + &"a".repeat(400);
        // Same 400-character prefix, different tails: truncation alone would
        // merge them.
        let a = format!("{long}#one");
        let b = format!("{long}#two");

        assert!(
            slug(&a).len() <= MAX_SLUG,
            "slug must fit: {}",
            slug(&a).len()
        );
        assert!(slug(&b).len() <= MAX_SLUG);
        assert_ne!(
            slug(&a),
            slug(&b),
            "two keys sharing a truncated prefix must not slug to one name"
        );
        // And the cap leaves room for `.md` plus a disambiguating suffix inside
        // NAME_MAX (255).
        assert!(slug(&a).len() + ".md".len() + 9 <= 255);
    }

    #[test]
    fn colliding_slugs_do_not_lose_a_concept() {
        // Different keys, identical slug. (Case is not a separate hazard here:
        // `slug` lowercases, so a case-only difference cannot survive into a
        // filename at all.)
        let a = explanation("sym:rust:a/b.rs#Thing", "fn", "Thing");
        let b = explanation("sym:rust:a-b.rs#thing", "fn", "thing");
        assert_eq!(
            slug(&a.node.key).to_ascii_lowercase(),
            slug(&b.node.key).to_ascii_lowercase(),
            "fixture must actually collide, or this test proves nothing"
        );

        let files = assemble(vec![concept(&a, "fn"), concept(&b, "fn")], "T", &[]);
        let concepts: Vec<&BundleFile> = files
            .iter()
            .filter(|f| !f.path.ends_with(INDEX_FILE) && !f.path.ends_with(LOG_FILE))
            .collect();
        assert_eq!(concepts.len(), 2, "both concepts must be written");

        let paths: std::collections::BTreeSet<String> = concepts
            .iter()
            .map(|f| f.path.to_ascii_lowercase())
            .collect();
        assert_eq!(
            paths.len(),
            2,
            "and to distinct files even when case is folded: {paths:?}"
        );
    }

    /// The same graph renders to the same bytes, whatever order it arrives in.
    ///
    /// Both fixtures are in the **same section** on purpose. An earlier version
    /// used an `adr` and a `fn`, which land in different directories — so each
    /// section held one member, ordering within a section was never exercised,
    /// and deleting the sort changed nothing. The test passed and guarded nothing.
    #[test]
    fn assembly_is_deterministic() {
        let a = explanation("sym:rust:a.rs#a", "fn", "a");
        let b = explanation("sym:rust:z.rs#z", "fn", "z");
        assert_eq!(
            section_for(&a.node.kind),
            section_for(&b.node.kind),
            "the fixtures must share a section, or ordering is not under test"
        );
        let once = assemble(vec![concept(&a, "fn"), concept(&b, "fn")], "T", &[]);
        let twice = assemble(vec![concept(&b, "fn"), concept(&a, "fn")], "T", &[]);
        assert_eq!(once, twice, "input order must not change the bundle");
    }

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

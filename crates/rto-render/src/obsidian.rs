//! The Obsidian-vault renderer: each graph node becomes a markdown note whose
//! edges are `[[wikilinks]]`, so the provenance-tagged graph is browsable in
//! Obsidian's graph view. Notes carry frontmatter `tags` (`roteiro/kind/*`,
//! `roteiro/lang/*`, `roteiro/status/*`) so the graph is colourable/filterable —
//! edge provenance is shown per-link in the body — surface the node's text as the
//! knowledge base, show an ADR's status, and (when the repository's web host is
//! known) a clickable **Source** link to the file.
//!
//! That text is the node's captured `meta.content` (a doc comment, PDF or image
//! text) *except* where the caller supplies a full `body` — which it does for
//! prose documents, because `meta.content` is an embedding budget and a note
//! rendered from it is the document capped at 1500 characters and collapsed onto
//! one line. See [`note_body`].
//!
//! A generated `_Home` note is the overview: what was
//! scanned, counts by kind, provenance breakdown, ADR statuses, intent-debt (with
//! the files it is densest in), an inventory of secret-**named** config keys and
//! their redaction state, and the most depended-on symbols by directed call
//! fan-in.
//! Built from the same [`Explanation`] the query surface returns, so the vault
//! and the CLI agree.

use std::fmt::Write as _;

use rto_graph::Explanation;

/// Filename of the generated overview note (sorts first in the file list).
pub const HOME_NOTE: &str = "_Home.md";

/// A rendered vault note: its filename (with `.md`) and markdown content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultNote {
    /// Filename including the `.md` extension.
    pub filename: String,
    /// Markdown content.
    pub content: String,
}

/// Map a node key to a filesystem- and wikilink-safe note stem that is **unique
/// per key even after case folding**.
///
/// A name is a lowercased, readable *hint* slugged from the key, followed by an
/// unconditional 64-bit FNV-1a hash of the whole, exact key written as 16 hex
/// digits — `<hint>-<16 hex digits>`. Characters outside `[a-z0-9._-]` collapse
/// to a single `-` in the hint; the hash carries everything the hint threw away.
///
/// **The hash is the unconditional part, not the hint.** A key of nothing but
/// separators slugs to an empty hint, and the name is then the bare 16 hex
/// digits — no hint, and no `-` to join it to. The two forms cannot be confused
/// for one another, which is what makes the exception safe rather than a second
/// naming rule: a hinted name is at least 18 characters and contains a `-`,
/// and a bare one is exactly 16 and contains none. That is argued again at the
/// branch itself, and asserted by
/// `every_name_carries_the_hash_however_short_the_key`.
///
/// # Why the hash is unconditional (issue #574)
///
/// It used to be applied only when the slug overran the filename limit, and the
/// slug alone was lossy twice over. Measured on this repository — 8,239 nodes
/// rendering to 8,135 notes, 104 of them silently overwritten:
///
/// | mechanism | lost | where |
/// | --- | --- | --- |
/// | every character outside the safe set becomes `-` and runs collapse, so `…cytoscape.min.js#$a` and `…cytoscape.min.js#a` are one name | 9 | everywhere |
/// | macOS and Windows fold filename case, so `…#A` and `…#a` are two *names* but one *file* | 95 | macOS, Windows |
///
/// The second mechanism is the trap. A lossless-but-case-sensitive encoding
/// fixes the 9, verifies clean on Linux CI, and still loses 95 notes on a Mac.
/// So the requirement is stated after folding:
///
/// ```text
/// lower(note_name(k1)) == lower(note_name(k2))  implies  k1 == k2
/// ```
///
/// This matters more than lossiness in a cache would, because the note names are
/// the vault's **only** stable interface: `reset_vault_dir` deletes and rebuilds
/// the whole directory on every render, so the one thing that survives a render
/// is a user's own note *outside* the vault linking in by name (issue #442).
///
/// # The trade taken
///
/// Two decisions, and what each bought:
///
/// **The hint is lowercased rather than case-preserved.** Case-preserving would
/// also satisfy the requirement — the hash differs for `#A` and `#a`, so the two
/// names differ in their suffix and stay distinct under folding. It was rejected
/// because lowercasing makes `note_name(k) == note_name(k).to_lowercase()` an
/// invariant of the function, and *that* collapses the folded property into the
/// literal one: there is then no way to write a version of this that is green on
/// Linux and lossy on macOS, which is the defect shape this repository keeps
/// finding. The cost is that `parseHTTPHeader` reads as `parsehttpheader`. That
/// is affordable precisely because the hint is a hint — once a 17-character
/// suffix is mandatory the name is not something anyone types from memory, so
/// its job is to be recognisable in a file list, not to be transcribed.
///
/// **Readability was spent, deliberately.** Every name grows by 17 characters and
/// hand-writing a link now needs Obsidian's autocomplete. The alternatives that
/// keep names short — hashing only the keys observed to collide — make the *set*
/// of collisions platform-dependent, so one key would get one filename on macOS
/// and another on Linux and a synced vault would churn. A name that is uglier
/// everywhere beats a name that is different per platform.
///
/// The mapping is not reversible (the hint is lossy and the hash is one-way), but
/// it does not need to be: every note's frontmatter carries `key:` verbatim, so
/// name → key is recoverable from the vault itself, which is the direction a
/// reader actually needs.
///
/// # What "unique" rests on
///
/// Equal names imply equal hashes, not equal keys — this is a 64-bit hash, not a
/// proof. Over this repository's 8,239 keys there is no collision, and the
/// birthday bound at that size is about 2e-12. Should one ever occur it is
/// *reported*, not silent: `NoteNames` in the render path claims every filename
/// case-insensitively and warns on a repeat. What is proved outright is the
/// folding half — the output is lowercase by construction, so case folding is the
/// identity on it.
#[must_use]
pub fn note_name(key: &str) -> String {
    // Keep the whole stem well under the 255-byte filename limit (leaving room
    // for ".md"). The hint is ASCII, so byte length equals char count and slicing
    // is safe.
    const MAX: usize = 200;
    // '-' plus the 16 hex digits of the hash.
    const SUFFIX: usize = 17;
    const HINT: usize = MAX - SUFFIX;

    let mut hint = String::with_capacity(key.len());
    let mut prev_dash = false;
    for c in key.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            hint.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            hint.push('-');
            prev_dash = true;
        }
    }
    let hint = hint.trim_matches('-');
    // Truncation is only ever cosmetic now: the hash, not the hint, is what keeps
    // a 300-character grouped `use` distinct from its neighbour.
    let hint = hint[..hint.len().min(HINT)].trim_end_matches('-');
    let hash = fnv1a64(key.as_bytes());
    if hint.is_empty() {
        // A key of nothing but separators. Bare hex, and it cannot be confused
        // with a hinted name: those are `<hint>-<16 hex>`, so at least 18
        // characters, and this is exactly 16 with no `-` in it.
        format!("{hash:016x}")
    } else {
        format!("{hint}-{hash:016x}")
    }
}

/// FNV-1a (64-bit) — a dependency-free, deterministic hash carrying everything
/// [`note_name`]'s hint discards. No cryptographic properties needed: nothing
/// here defends against a chosen collision, only against an accidental one.
///
/// 64 bits rather than fewer because the cost of a collision is exactly the
/// defect this suffix exists to fix — a note silently overwritten. At 8k keys a
/// 32-bit hash collides about 0.8% of the time and a 48-bit one about 1e-5;
/// 64 bits is 2e-12, and stays under 1e-10 for a workspace vault an order of
/// magnitude larger.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Emit `value` as a YAML **double-quoted** scalar, `"`-delimited and escaped so
/// it parses back to exactly `value`.
///
/// The one escaping rule for this module's frontmatter. It exists because the
/// three hand-rolled variants it replaced disagreed with each other — `key:` and
/// `project:` turned a `"` into an apostrophe, and `path:` escaped nothing — and
/// two of the three could emit YAML that does not mean what it says:
///
/// | value | was emitted | parsed back as |
/// | --- | --- | --- |
/// | `foo\bar` | `"foo\bar"` | `foo<BS>ar` — `\b` is YAML's **backspace** escape |
/// | `foo\dir` | `"foo\dir"` | *parse error* — `\d` is not a YAML escape |
/// | `say"hi".rs` | `"say"hi".rs"` | *parse error* — the scalar ends at the `"` |
///
/// The first is the dangerous one: seven characters silently become six, and
/// nothing anywhere reports it. The other two cost the reader every property on
/// the note, because Obsidian parses this block as the note's properties and a
/// block that does not parse yields no properties at all rather than an error.
///
/// All three inputs are legal path components on Linux and macOS. None occurs in
/// this repository today, so this is a latent defect rather than an observed one.
///
/// Escapes, per YAML 1.2 §7.3.1: the two structural characters `\` and `"`, then
/// anything a parser is not obliged to accept literally — C0 controls, `DEL`, the
/// C1 range, and the three separators (`U+2028`, `U+2029`, `U+FEFF`) that some
/// parsers treat as line breaks. Short escapes where YAML defines one, so the
/// common cases stay readable, and `\uXXXX` otherwise.
fn yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            '\u{0}' => out.push_str(r"\0"),
            '\u{7}' => out.push_str(r"\a"),
            '\u{8}' => out.push_str(r"\b"),
            '\u{b}' => out.push_str(r"\v"),
            '\u{c}' => out.push_str(r"\f"),
            '\u{1b}' => out.push_str(r"\e"),
            // Everything else a YAML parser may reject or fold: the rest of C0,
            // DEL, the C1 range, and the separators that can read as line breaks.
            c if (c < ' ')
                || c == '\u{7f}'
                || ('\u{80}'..='\u{9f}').contains(&c)
                || matches!(c, '\u{2028}' | '\u{2029}' | '\u{feff}') =>
            {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Emit `value` in YAML **plain** (unquoted) style when that round-trips, and as
/// [`yaml_double_quoted`] when it would not.
///
/// For the frontmatter fields that are written bare today — `kind`, `lang`,
/// `status`. Those are constrained by *today's* producers (an ADR's status is
/// validated against the house states; kinds and languages come from extraction),
/// but `roteiro load` installs a caller-supplied graph artifact whose nodes carry
/// whatever JSON they carry, so "the producer is careful" is not a property this
/// renderer can rely on. A `status:` of `Accepted: superseded by 0012` emitted
/// bare is a parse error, and `Accepted # pending` silently truncates to
/// `Accepted`.
///
/// Escalating only when needed is what keeps the bytes of an existing vault
/// unchanged — every `kind`, `lang` and `status` in this repository is plain-safe
/// and stays bare. [`is_plain_safe`] is deliberately stricter than YAML's plain
/// grammar for the same reason it is safe: a value it rejects is merely quoted.
fn yaml_scalar(value: &str) -> String {
    if is_plain_safe(value) {
        value.to_owned()
    } else {
        yaml_double_quoted(value)
    }
}

/// Whether `value` can be written as a bare YAML scalar and read back unchanged.
///
/// A conservative allowlist rather than YAML's actual plain-scalar grammar, which
/// is subtle enough (indicator characters, `: ` and ` #` only in some positions,
/// leading and trailing space, implicit typing) that implementing it is how the
/// bug this replaces gets written a second time. Getting this wrong in the
/// strict direction costs a pair of quotation marks; getting it wrong in the
/// permissive direction costs the note's properties.
///
/// So: a leading ASCII letter, then letters, digits, `_`, `-`, `.` and `/` — which
/// covers every kind, language and status this renderer emits — and never a word
/// YAML resolves to a boolean or null. That last exclusion is not hypothetical:
/// `no` is the ISO 639-1 code for Norwegian, and YAML 1.1 parsers read a bare `no`
/// as `false`.
fn is_plain_safe(value: &str) -> bool {
    const NOT_STRINGS: [&str; 11] = [
        "true", "false", "yes", "no", "on", "off", "null", "nil", "none", "y", "n",
    ];
    !value.is_empty()
        && value.starts_with(|c: char| c.is_ascii_alphabetic())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && !NOT_STRINGS.contains(&value.to_ascii_lowercase().as_str())
}

/// Which vault a note is being rendered into: a single project's, or one member
/// of a **workspace** vault spanning several repositories.
///
/// This is the whole of the workspace-vault naming rule, in one place. Node keys
/// are **repository-relative** (`file:README.md` names no repo), so every member
/// of a workspace produces the same note name for its `README.md` and one would
/// silently overwrite the rest. Qualifying the key with its project fixes that.
///
/// [`VaultScope::PROJECT`] (`project: None`) is not a degenerate case but the
/// contract: it makes every name in this module reduce to exactly [`note_name`]
/// of the bare key, with nothing qualified and no `project:` frontmatter.
///
/// That reduction is *still* the promise; what it no longer implies is stability
/// against `main`. #570 could say "a single-project vault's names do not move",
/// because the only thing moving them would have been workspace qualification.
/// #574 moves them all, on purpose: the old names were not injective under
/// filename case folding and the vault lost 104 notes to that. The promise here
/// was always about **this axis** — turning workspace mode on must not rename a
/// project's notes — and it holds unchanged. See [`note_name`] for the rename and
/// what it bought.
#[derive(Debug, Clone, Copy)]
pub struct VaultScope<'a> {
    /// The member project this note belongs to, qualifying its name as
    /// `<project>::<key>` — the same form ADR-0009's cross-repo links already use.
    /// `None` ⇒ a single-project vault, and names are unqualified exactly as
    /// before.
    pub project: Option<&'a str>,
    /// The workspace's member project names. An external-ref placeholder whose
    /// target names one of these is a cross-repo edge the vault can actually
    /// follow, so it is rendered as a link straight to that member's note. Empty
    /// for a single-project vault.
    pub members: &'a std::collections::BTreeSet<String>,
}

/// The empty member set backing [`VaultScope::PROJECT`] — a single-project vault
/// has no other members to resolve a cross-repo reference against.
static NO_MEMBERS: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

impl VaultScope<'_> {
    /// A single-project vault: names are unqualified, and no cross-repo reference
    /// resolves. Every name this produces is byte-identical to [`note_name`] of
    /// the bare key — see the type's documentation for why that reduction is
    /// load-bearing, and for what it does *not* promise.
    pub const PROJECT: Self = Self {
        project: None,
        members: &NO_MEMBERS,
    };
}

impl Default for VaultScope<'_> {
    fn default() -> Self {
        Self::PROJECT
    }
}

impl VaultScope<'_> {
    /// Whether an external-ref placeholder `key` is one this vault resolves for
    /// itself — its target names a member, so every edge to it points at the real
    /// note and the placeholder need not be rendered at all.
    ///
    /// The single rule behind both halves of that: [`link_target`] redirects
    /// exactly the keys this accepts, and the caller skips writing exactly the
    /// notes this accepts. They cannot disagree.
    #[must_use]
    pub fn redirects_external_ref(&self, key: &str) -> bool {
        key.strip_prefix("extref:")
            .and_then(rto_graph::parse_qualified)
            .is_some_and(|(project, _)| self.members.contains(project))
    }
}

/// The note name for a node `key` owned by `scope`'s project.
///
/// In a single-project vault (`scope.project == None`) this *is* [`note_name`].
/// In a workspace vault it is [`note_name`] of the project-qualified key
/// `<project>::<key>` — reusing ADR-0009's qualified form rather than inventing a
/// second one, which is what lets a cross-repo external-ref target (already
/// stored qualified) map to its note by the very same call.
#[must_use]
pub fn scoped_note_name(scope: &VaultScope<'_>, key: &str) -> String {
    match scope.project {
        None => note_name(key),
        Some(project) => note_name(&format!("{project}::{key}")),
    }
}

/// The note an edge pointing at `key` should link to.
///
/// Almost always [`scoped_note_name`]. The exception is the one cross-repo edge
/// the graph already models: a spoke's inferred link to a hub is stored as an
/// edge to a **local external-ref placeholder** (`extref:<project>::<key>`,
/// [`rto_graph::external_ref_key`]) because store integrity requires both ends of
/// an edge in one store. A workspace vault holds both repos' notes, so when the
/// placeholder's target names a member the link is pointed at the **real** note
/// instead of the stand-in.
///
/// This invents no edge. It renders the edge that is there, following the
/// placeholder exactly as [`rto_graph::Workspace::follow_external_ref`] does at
/// query time — the cross-repo graph has only ever been *rendered* one repo at a
/// time.
fn link_target(scope: &VaultScope<'_>, key: &str) -> String {
    if scope.redirects_external_ref(key) {
        // `note_name(qualified)` is by construction the same string
        // `scoped_note_name` produces for that member's own copy of the node.
        // `strip_prefix`, not `trim_start_matches`: the latter strips the prefix
        // repeatedly, which would mangle a target that legitimately starts with it.
        return note_name(key.strip_prefix("extref:").unwrap_or(key));
    }
    scoped_note_name(scope, key)
}

/// Render a node's [`Explanation`] into an Obsidian note: YAML frontmatter (with
/// `tags` for the graph view and an ADR's `status`), a clickable **Source** link
/// (when `source_base` — a web "blob" base like
/// `https://github.com/org/repo/blob/<sha>` — is known and the node has a path),
/// the content as the knowledge base, and its edges as provenance-labelled
/// wikilinks.
///
/// `body` is the node's **full source text**, which only the caller can fetch:
/// this function is a pure function of the `Explanation`, and an `Explanation`
/// carries no repository, store or blob. When it is `Some`, it replaces
/// `meta.content` in the note's `## Content` section — see [`note_body`] for why
/// replacing is the only correct combination of the two.
#[must_use]
pub fn render_note(ex: &Explanation, source_base: Option<&str>, body: Option<&str>) -> VaultNote {
    render_note_scoped(ex, source_base, body, &VaultScope::PROJECT)
}

/// [`render_note`], for one member of a **workspace** vault: identical except
/// that the note's own name and every link it emits are resolved through `scope`
/// (see [`VaultScope`]).
///
/// With [`VaultScope::PROJECT`] this is [`render_note`] byte for byte, which is
/// how the single-project vault's compatibility promise is kept by construction
/// rather than by a parallel code path that has to be kept in step.
#[must_use]
pub fn render_note_scoped(
    ex: &Explanation,
    source_base: Option<&str>,
    body: Option<&str>,
    scope: &VaultScope<'_>,
) -> VaultNote {
    let meta = &ex.meta;
    let status = meta.get("status").and_then(|v| v.as_str());
    let content = note_body(meta.get("content").and_then(|v| v.as_str()), body);

    let mut c = String::new();
    c.push_str("---\n");
    let _ = writeln!(c, "key: {}", yaml_double_quoted(&ex.node.key));
    let _ = writeln!(c, "kind: {}", yaml_scalar(ex.node.kind.as_str()));
    // Which member this note came from. Absent in a single-project vault, where
    // it would be one constant repeated on every note — and where adding it would
    // change every note's bytes.
    if let Some(project) = scope.project {
        let _ = writeln!(c, "project: {}", yaml_double_quoted(project));
    }
    if let Some(path) = &ex.node.path {
        let _ = writeln!(c, "path: {}", yaml_double_quoted(path));
    }
    if let Some(lang) = &ex.node.lang {
        let _ = writeln!(c, "lang: {}", yaml_scalar(lang));
    }
    if let Some(status) = status {
        let _ = writeln!(c, "status: {}", yaml_scalar(status));
    }
    // Nested tags group in Obsidian's tag pane and colour the graph view.
    c.push_str("tags:\n");
    let _ = writeln!(c, "  - roteiro/kind/{}", tag_slug(&ex.node.kind));
    // Colours the graph view by member, which is the one thing a workspace vault
    // is for and a per-project vault has no use for.
    if let Some(project) = scope.project {
        let _ = writeln!(c, "  - roteiro/project/{}", tag_slug(project));
    }
    if let Some(lang) = &ex.node.lang {
        let _ = writeln!(c, "  - roteiro/lang/{}", tag_slug(lang));
    }
    if let Some(status) = status {
        let _ = writeln!(c, "  - roteiro/status/{}", tag_slug(status));
    }
    c.push_str("---\n\n");

    let _ = writeln!(c, "# {}", ex.node.name);
    if let Some(status) = status {
        let _ = writeln!(c, "\n> **Status:** {status}");
    }

    // A clickable link to the file this node comes from. An absolute URL, so it
    // works from the downloaded vault too (which has no repo files beside it).
    if let (Some(base), Some(path)) = (source_base, ex.node.path.as_deref()) {
        let _ = writeln!(
            c,
            "\n**Source:** [`{path}`]({}/{path})",
            base.trim_end_matches('/')
        );
    }

    // The knowledge base: the full source text, or the captured doc comment /
    // prose / PDF / image text.
    if let Some(content) = content.map(str::trim).filter(|s| !s.is_empty()) {
        c.push_str("\n## Content\n\n");
        c.push_str(content);
        c.push('\n');
    }

    if !ex.outgoing.is_empty() {
        c.push_str("\n## Outgoing\n\n");
        for e in &ex.outgoing {
            let _ = writeln!(
                c,
                "- {} ({}){} → [[{}]]",
                e.kind,
                e.provenance,
                confidence(e.confidence),
                link_target(scope, &e.node)
            );
        }
    }
    if !ex.incoming.is_empty() {
        c.push_str("\n## Incoming\n\n");
        for e in &ex.incoming {
            let _ = writeln!(
                c,
                "- [[{}]] {} ({}){} →",
                link_target(scope, &e.node),
                e.kind,
                e.provenance,
                confidence(e.confidence)
            );
        }
    }

    VaultNote {
        filename: format!("{}.md", scoped_note_name(scope, &ex.node.key)),
        content: c,
    }
}

/// Choose the text a note shows: the caller's full `body` when it has one, else
/// the node's stored `content`.
///
/// The two are **not** complementary, they are the same text at two fidelities,
/// so a note shows one of them and never both. `meta.content` is an embedding
/// budget — extraction caps it (1500 chars) and collapses every whitespace run to
/// a single space, which is right for a store that ships with the graph and wrong
/// for a note: a 23 KB document arrives as one 1500-character line with every
/// heading, table and code fence flattened into it. Where the caller can supply
/// the source, that is what a reader wants; appending the capped rendering
/// underneath it would only restate its first 6% badly.
fn note_body<'a>(content: Option<&'a str>, body: Option<&'a str>) -> Option<&'a str> {
    body.or(content)
}

/// `" (0.82)"` for an inferred edge's confidence, else empty.
fn confidence(c: Option<f64>) -> String {
    c.map_or_else(String::new, |c| format!(" ({c:.2})"))
}

/// A tag-safe slug: lowercase, non-alphanumeric runs → `-`. Keeps Obsidian tags
/// (`roteiro/kind/adr-section`) valid and stable.
fn tag_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}

/// One ADR in the overview, with its lifecycle status.
#[derive(Debug, Clone)]
pub struct AdrEntry {
    /// The ADR node key (`adr:<id>`).
    pub key: String,
    /// The ADR title.
    pub name: String,
    /// Lifecycle status (`Accepted`, …), if recorded.
    pub status: Option<String>,
}

/// The `_Home` overview's config-secret inventory figures.
///
/// Counts and file paths only — deliberately not the key names, which belong in
/// `roteiro config-secrets` where the caveat can be stated at length. A vault note
/// is read casually and out of context, which is exactly the wrong place for a
/// list that looks like a secret scan's output.
#[derive(Debug, Clone, Default)]
pub struct ConfigSecretSummary {
    /// Config keys whose **name** matched the secret-name heuristic.
    pub secret_named: usize,
    /// Of those, how many had their value redacted before persistence.
    pub redacted: usize,
    /// Of those, how many are declared in code with no literal value.
    pub declared: usize,
    /// Of those, how many carry an unredacted value. Expected to be zero.
    pub unredacted: usize,
    /// Distinct files carrying at least one secret-named key, ordered and capped
    /// by the caller.
    pub files: Vec<String>,
}

/// One file in the `_Home` overview's intent-debt density table.
#[derive(Debug, Clone)]
pub struct DensityEntry {
    /// Repository-relative path, used for both the wikilink and the label.
    pub path: String,
    /// Retained markers in the file.
    pub markers: u32,
    /// The file's length in lines — the denominator.
    pub lines: u32,
    /// Markers per 1,000 lines.
    pub per_kloc: f64,
}

/// One node in the `_Home` overview's directed-coupling table.
#[derive(Debug, Clone)]
pub struct CouplingEntry {
    /// The node key, for the wikilink.
    pub key: String,
    /// The symbol name.
    pub name: String,
    /// Distinct callers.
    pub fan_in: u32,
    /// Distinct callees.
    pub fan_out: u32,
}

/// Aggregate figures for the vault's `_Home` overview note.
#[derive(Debug, Clone, Default)]
pub struct VaultSummary {
    /// Name of the scanned project (repository directory).
    pub project: String,
    /// Total node and edge counts.
    pub total_nodes: usize,
    /// Total edge count.
    pub total_edges: usize,
    /// `(kind, count)` for each node kind, most-frequent first.
    pub node_counts: Vec<(String, usize)>,
    /// `(provenance, edge count)` — `derived` / `authored` / `inferred`.
    pub edge_provenance: Vec<(String, usize)>,
    /// The ADRs, with status.
    pub adrs: Vec<AdrEntry>,
    /// `(category, count)` of intent-debt markers.
    pub debt: Vec<(String, usize)>,
    /// The files where that debt is most **concentrated**, already ranked and
    /// capped by the caller. Empty when the graph has no markers, or when no
    /// file carrying one has a recorded length.
    pub densest_files: Vec<DensityEntry>,
    /// Secret-named config keys and their redaction state. `None` when the graph
    /// holds no secret-named config key — the section is then absent rather than
    /// rendering a row of zeroes, which would read as a clean bill of health this
    /// lens cannot give.
    pub config_secrets: Option<ConfigSecretSummary>,
    /// The most depended-on symbols by **directed** call fan-in, already ranked
    /// and capped by the caller. Empty when the graph has no `calls` edges.
    pub most_called: Vec<CouplingEntry>,
    /// Web root of the repository (`https://host/owner/repo`), if derivable from
    /// the git remote — for a "Repository" link in the overview.
    pub repo_url: Option<String>,
    /// Hex commit the graph was rendered from, for a permalink note.
    pub commit: Option<String>,
}

/// Render the vault's overview note: what was scanned, the structure by kind,
/// the provenance breakdown, the decisions (ADRs) and their status, the
/// intent-debt summary, and how to navigate. The entry point for the vault.
#[must_use]
pub fn render_home(s: &VaultSummary) -> VaultNote {
    let mut c = String::new();
    c.push_str("---\ntags:\n  - roteiro/home\n---\n\n");
    let _ = writeln!(c, "# {} — knowledge graph", s.project);
    c.push_str(
        "\n*A browsable snapshot of this codebase as one **knowledge graph**, \
         generated by [Roteiro](https://roteiro.dev). Every symbol, document and \
         decision is a note, linked to the things it relates to.*\n",
    );
    c.push_str(HOW_TO_READ);
    let _ = writeln!(
        c,
        "\n**{} nodes**, **{} edges** across the project.",
        s.total_nodes, s.total_edges
    );
    write_repo_line(&mut c, s);
    write_summary_sections(&mut c, s, &VaultScope::PROJECT, 2);
    c.push_str(NAVIGATING);

    VaultNote {
        filename: HOME_NOTE.to_owned(),
        content: c,
    }
}

/// The "how to read a note" paragraph. Shared verbatim by the single-project and
/// workspace overviews — the notes themselves are identical in both, so a reader
/// who learns the format once has learned it for either.
const HOW_TO_READ: &str = "\n**How to read it.** Open any note to see what a thing is, the intent or \
     docs behind it (its **Content**), where it lives (its **Source** link), \
     and how it connects (**Outgoing**/**Incoming** links). Each link is \
     labelled with how the fact was established — `derived` (extracted from \
     code), `authored` (human intent: ADRs, blueprints, annotations), or \
     `inferred` (a scored suggestion). Open Obsidian's **graph view** to see \
     the whole thing at once.\n";

/// The closing navigation section.
const NAVIGATING: &str = "\n## Navigating this vault\n\n\
     - Open the **graph view** to see the whole codebase; notes are coloured/\
     filterable by their `roteiro/kind/*`, `roteiro/lang/*` and \
     `roteiro/status/*` tags.\n\
     - Each note carries its captured **content** (doc comments, prose, PDF/\
     image text) and its provenance-labelled incoming/outgoing links.\n\
     - Start from an ADR above, or search the tag pane for a kind.\n";

/// `**Repository:** …` — the web root and the commit the graph was rendered from.
fn write_repo_line(c: &mut String, s: &VaultSummary) {
    if let Some(repo) = &s.repo_url {
        let _ = write!(c, "\n**Repository:** [{repo}]({repo})");
        if let Some(commit) = &s.commit {
            let short = &commit[..commit.len().min(12)];
            let _ = write!(c, " · rendered at commit `{short}`");
        }
        c.push('\n');
    }
}

/// Every aggregate the overview carries for **one project**: structure by kind,
/// provenance, ADRs, intent debt (and where it is densest), the config-secret
/// inventory and directed call coupling.
///
/// Factored out of [`render_home`] so a workspace vault's per-member section is
/// *the same code*, not a reimplementation that can drift: the promise in issue
/// #442 is that today's per-project view stays a **subset** of the workspace one
/// rather than a casualty of it. `level` is the markdown heading depth — 2 for a
/// single-project `_Home`, 3 inside a member's section — and `scope` decides
/// whether the wikilinks point at bare or project-qualified notes.
fn write_summary_sections(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, level: usize) {
    let hd = &"#".repeat(level);
    let sub = &"#".repeat(level + 1);
    write_structure(c, s, hd);
    write_decisions(c, s, scope, hd);
    write_debt(c, s, scope, hd, sub);
    write_config_secrets(c, s, scope, hd);
    write_coupling(c, s, scope, hd);
}

/// `Structure` (nodes by kind) and `Provenance` (edges by how they were established).
fn write_structure(c: &mut String, s: &VaultSummary, hd: &str) {
    let _ = write!(c, "\n{hd} Structure\n\n| Kind | Count |\n| --- | --- |\n");
    for (kind, n) in &s.node_counts {
        let _ = writeln!(c, "| {kind} | {n} |");
    }

    if !s.edge_provenance.is_empty() {
        let _ = write!(
            c,
            "\n{hd} Provenance\n\n| Provenance | Edges |\n| --- | --- |\n"
        );
        for (prov, n) in &s.edge_provenance {
            let _ = writeln!(c, "| {prov} | {n} |");
        }
    }
}

/// `Decisions (ADRs)` — the recorded decisions and their lifecycle status.
fn write_decisions(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str) {
    let _ = write!(c, "\n{hd} Decisions (ADRs)\n\n");
    if s.adrs.is_empty() {
        c.push_str("*No ADRs found.*\n");
    } else {
        for adr in &s.adrs {
            let status = adr.status.as_deref().unwrap_or("—");
            let _ = writeln!(
                c,
                "- **{status}** — [[{}|{}]]",
                scoped_note_name(scope, &adr.key),
                adr.name
            );
        }
    }
}

/// `Intent debt` — the marker categories, and the files the debt is densest in.
fn write_debt(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str, sub: &str) {
    let _ = write!(c, "\n{hd} Intent debt\n\n");
    if s.debt.is_empty() {
        c.push_str("*None recorded.*\n");
    } else {
        c.push_str("| Category | Count |\n| --- | --- |\n");
        for (cat, n) in &s.debt {
            let _ = writeln!(c, "| {cat} | {n} |");
        }
    }

    if !s.densest_files.is_empty() {
        let _ = write!(
            c,
            "\n{sub} Densest files (markers per 1,000 lines)\n\n\
             *Where the debt above is concentrated, rather than where there is \
             most of it — a raw count ranks the biggest file first by \
             construction. The denominator is file length: every line, blanks and \
             comments included, not source lines of code. Prose matches (`for \
             now`, `tbd`) count too, so a design document can rank high.*\n\n"
        );
        c.push_str("| File | Markers | Lines | Per 1k |\n| --- | --- | --- | --- |\n");
        for e in &s.densest_files {
            let _ = writeln!(
                c,
                "| [[{}\\|{}]] | {} | {} | {:.2} |",
                scoped_note_name(scope, &format!("file:{}", e.path)),
                e.path,
                e.markers,
                e.lines,
                e.per_kloc
            );
        }
    }
}

/// `Config keys named like secrets` — an inventory and its unconditional caveat.
fn write_config_secrets(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str) {
    if let Some(cs) = &s.config_secrets {
        let _ = write!(c, "\n{hd} Config keys named like secrets\n\n");
        let _ = writeln!(
            c,
            "**{}** secret-named config key(s): {} redacted before storage, {} \
             declared in code without a value, {} unredacted.",
            cs.secret_named, cs.redacted, cs.declared, cs.unredacted
        );
        if cs.unredacted > 0 {
            let _ = writeln!(
                c,
                "\n> [!warning] {} key(s) carry an **unredacted** value. Extraction \
                 always redacts, so these came from an import layer — inspect the \
                 importing tool, not this repository.",
                cs.unredacted
            );
        }
        if !cs.files.is_empty() {
            c.push_str("\nIn:\n");
            for path in &cs.files {
                let _ = writeln!(
                    c,
                    "- [[{}\\|{path}]]",
                    scoped_note_name(scope, &format!("file:{path}"))
                );
            }
        }
        // The caveat is unconditional and comes last, so it is the final thing read
        // in this section. A vault note is browsed out of context; this is exactly
        // where "config keys named like secrets" would otherwise be misread as a
        // secret scan that came back clean.
        c.push_str(
            "\n*An inventory of config keys whose **names** look secret, not a secret \
             scan. Values are redacted before they are stored, so this reports that \
             such keys exist and were redacted — never a value. It cannot see a \
             hardcoded credential in source code, cannot judge whether a value is \
             valid, and cannot tell a real secret from a placeholder. A credential \
             under an innocuous key name (`dsn`, `endpoint`) does not appear here at \
             all, so this section being small says nothing about whether this \
             repository leaks secrets.*\n",
        );
    }
}

/// `Most depended-on (call fan-in)` — directed call coupling, capped.
fn write_coupling(c: &mut String, s: &VaultSummary, scope: &VaultScope<'_>, hd: &str) {
    if !s.most_called.is_empty() {
        let _ = write!(
            c,
            "\n{hd} Most depended-on (call fan-in)\n\n\
             *Distinct callers and callees over `calls` edges — direction kept, so \
             \"everything calls this\" and \"this calls everything\" are not the same \
             row. Call targets are resolved by simple name, so a short, generically-\
             named function can absorb every call to that name: read a large fan-in on \
             one as a question, not a finding.*\n\n"
        );
        c.push_str("| Symbol | Called by | Calls |\n| --- | --- | --- |\n");
        for e in &s.most_called {
            let _ = writeln!(
                c,
                "| [[{}\\|{}]] | {} | {} |",
                scoped_note_name(scope, &e.key),
                e.name,
                e.fan_in,
                e.fan_out
            );
        }
    }
}

/// One cross-repo edge the workspace vault can actually follow: a spoke's node
/// linking to a hub's, through the external-ref placeholder ADR-0009 persists.
///
/// Collected by the caller, which has every member's store open; the renderer
/// only lays them out. Nothing here is a new edge — these are the `inferred`
/// links `roteiro links` already reports, rendered for the first time.
#[derive(Debug, Clone)]
pub struct CrossLink {
    /// The member the edge starts in.
    pub from_project: String,
    /// The source node's key, within `from_project`.
    pub from_key: String,
    /// The source node's display name.
    pub from_name: String,
    /// The edge kind (`links`, …).
    pub kind: String,
    /// Confidence, for an `inferred` edge.
    pub confidence: Option<f64>,
    /// Whether this link was **declared** (`[[links]]` in the source repo's
    /// config, ADR-0009) rather than inferred by key matching.
    ///
    /// The distinction is the whole of ADR-0009's `authored → gold,
    /// inferred → slate`: a declaration is a statement of intent by someone who
    /// knows the topology, a match is a candidate. Until #573 the vault could not
    /// draw it, because nothing persisted an authored cross-repo edge — so this
    /// section carried a blanket caveat saying every row was a candidate.
    pub authored: bool,
    /// The project-qualified target, `<project>::<key>` (ADR-0009).
    pub to_qualified: String,
    /// Whether `to_qualified`'s project is a member of this workspace — and so
    /// whether the link resolves to a note in this vault, or dangles because the
    /// target repository is outside it.
    pub resolves: bool,
}

/// Aggregate figures for a **workspace** vault's `_Home` overview: the members,
/// each with exactly the aggregates a single-project `_Home` carries, plus the
/// cross-repo links between them.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceSummary {
    /// The workspace name (`--workspace-name`).
    pub name: String,
    /// One entry per member repository, in stable name order. Each is the very
    /// same [`VaultSummary`] a per-project vault would render.
    pub members: Vec<VaultSummary>,
    /// Cross-repo links between members, already ordered and capped by the caller.
    pub cross_links: Vec<CrossLink>,
    /// Cross-repo links found in total, which `cross_links` may be a capped view
    /// of — so the section can say what it is not showing.
    pub cross_links_total: usize,
}

/// Render a **workspace** vault's overview: the members and their scale, the
/// cross-repo links between them, and then each member's own aggregates —
/// structure, provenance, ADRs, intent debt, config-secret inventory and call
/// coupling — under its own heading.
///
/// The per-member sections are rendered by the same [`write_summary_sections`]
/// the single-project `_Home` uses, so the existing view is a **subset** of this
/// one: someone who came for their repository's coupling and debt tables finds
/// them, rather than a workspace total that averages them away.
#[must_use]
pub fn render_workspace_home(ws: &WorkspaceSummary) -> VaultNote {
    let members: std::collections::BTreeSet<String> =
        ws.members.iter().map(|m| m.project.clone()).collect();

    let mut c = String::new();
    c.push_str("---\ntags:\n  - roteiro/home\n  - roteiro/workspace\n---\n\n");
    let _ = writeln!(c, "# {} — workspace knowledge graph", ws.name);
    c.push_str(
        "\n*A browsable snapshot of a whole **workspace** as one **knowledge \
         graph**, generated by [Roteiro](https://roteiro.dev). Every symbol, \
         document and decision in every member repository is a note, linked to the \
         things it relates to — including across repositories.*\n",
    );
    c.push_str(HOW_TO_READ);
    // The example is *rendered* by `note_name` rather than spelled out. A
    // hand-written spelling of this sentence survived #574 unchanged, so every
    // vault v2.0.0 built stated the pre-#574 naming rule — on the first page a
    // reader opens — while `note_name` was writing something else. This is the
    // one copy that lives in the crate defining the rule, so it can simply ask:
    // a derived example cannot drift, and a spelled one already has.
    //
    // The *key* it names has to be real too, or the fix trades one false
    // sentence in `_Home` for another: `<member>::file:README.md` was fabricated
    // from the member list, and workspace membership does not require a README.
    // A cross-repo link's **source** end is the strongest key available here —
    // `from_project` is a member by definition and `from_key` is a node in that
    // member's own store, which the Cross-repo links table below already links
    // to by name. The *target* end will not do: `resolves == false` means the
    // target repository is outside this vault, so `to_qualified` names no note
    // here — the same false claim one remove away.
    //
    // With no cross-repo links there is no key this function can prove is a
    // node, so the sentence says nothing rather than inventing one. The rule it
    // states is complete without an example; only the illustration is lost.
    let example = ws.cross_links.first().map_or_else(String::new, |l| {
        let key = format!("{}::{}", l.from_project, l.from_key);
        format!(" Here, `{key}` is the note `{}.md`.", note_name(&key))
    });
    let _ = writeln!(
        c,
        "\n**Every note is keyed `<project>::<key>`**, because a node key is \
         repository-relative: the same path or symbol can occur in more than one \
         member, and without the project the second note would overwrite the \
         first. A note's *filename* is derived from that key — a readable \
         lowercase hint, then a hash of the whole key — so no filename contains \
         `::`.{example} Filter the graph view by a member's `roteiro/project/*` \
         tag to see one repository at a time."
    );

    let total_nodes: usize = ws.members.iter().map(|m| m.total_nodes).sum();
    let total_edges: usize = ws.members.iter().map(|m| m.total_edges).sum();
    let _ = writeln!(
        c,
        "\n**{total_nodes} nodes**, **{total_edges} edges** across **{}** member \
         repositor{}.",
        ws.members.len(),
        if ws.members.len() == 1 { "y" } else { "ies" }
    );

    c.push_str("\n## Members\n\n| Project | Nodes | Edges | Repository | Commit |\n| --- | --- | --- | --- | --- |\n");
    for m in &ws.members {
        let repo = m
            .repo_url
            .as_ref()
            .map_or_else(|| "—".to_owned(), |u| format!("[{u}]({u})"));
        let commit = m.commit.as_ref().map_or_else(
            || "—".to_owned(),
            |c| format!("`{}`", &c[..c.len().min(12)]),
        );
        let _ = writeln!(
            c,
            "| [[#{}\\|{}]] | {} | {} | {repo} | {commit} |",
            m.project, m.project, m.total_nodes, m.total_edges
        );
    }
    c.push_str(
        "\n*The `Repository` and `Commit` columns say where each member came from \
         and what was read. They are **not** a replication manifest — reconstructing \
         a workspace from a vault is issue #442 part 2, and nothing here is designed \
         to be handed to someone else.*\n",
    );

    write_cross_links(&mut c, ws);

    for m in &ws.members {
        let _ = writeln!(c, "\n## {}", m.project);
        let _ = writeln!(
            c,
            "\n**{} nodes**, **{} edges** in this member.",
            m.total_nodes, m.total_edges
        );
        write_repo_line(&mut c, m);
        let scope = VaultScope {
            project: Some(&m.project),
            members: &members,
        };
        write_summary_sections(&mut c, m, &scope, 3);
    }

    c.push_str(NAVIGATING);

    VaultNote {
        filename: HOME_NOTE.to_owned(),
        content: c,
    }
}

/// The `## Cross-repo links` section: the edges that only a workspace vault can
/// show, and the honest statement of what is missing from them.
fn write_cross_links(c: &mut String, ws: &WorkspaceSummary) {
    c.push_str("\n## Cross-repo links\n\n");
    if ws.cross_links.is_empty() {
        c.push_str(
            "*None. These are the `inferred` cross-repo links `roteiro links \
             --infer --write` persists (ADR-0009); a workspace whose members have \
             never been inferred over has none recorded yet.*\n",
        );
        return;
    }
    // The caveat is per-row now, because the two provenances are no longer the
    // same claim: an **authored** row was declared by someone who knows the
    // topology, an **inferred** row is a scored guess. Saying "these are all
    // candidates" over a table containing declarations would understate the
    // declarations exactly as saying nothing would overstate the matches.
    let authored = ws.cross_links.iter().filter(|l| l.authored).count();
    let inferred = ws.cross_links.len() - authored;
    let _ = writeln!(
        c,
        "*A spoke's config key and the hub key it corresponds to, across \
         repositories — the one thing a per-project vault structurally cannot \
         show. **{authored} declared** (`[[links]]`, ADR-0009 — a statement of \
         intent) and **{inferred} inferred** (`roteiro links --infer --write` — \
         read those as candidate correspondences).*\n"
    );
    c.push_str("| From | | To | Kind |\n| --- | --- | --- | --- |\n");
    for l in &ws.cross_links {
        let from_scope = VaultScope {
            project: Some(&l.from_project),
            members: &NO_MEMBERS,
        };
        let to = if l.resolves {
            format!("[[{}\\|{}]]", note_name(&l.to_qualified), l.to_qualified)
        } else {
            // Outside this workspace: there is no note to link to, and a wikilink
            // to a note that does not exist reads in Obsidian as one that is
            // merely unwritten.
            format!("`{}` *(outside this workspace)*", l.to_qualified)
        };
        // `declared` rather than a confidence score: an authored link carries no
        // score by construction, so an empty cell there would read as "confidence
        // unknown" instead of "not that kind of claim".
        let how = if l.authored {
            " *(declared)*".to_owned()
        } else {
            confidence(l.confidence)
        };
        let _ = writeln!(
            c,
            "| [[{}\\|{}]] | {} | {to} | {}{how} |",
            scoped_note_name(&from_scope, &l.from_key),
            l.from_name,
            l.from_project,
            l.kind,
        );
    }
    if ws.cross_links_total > ws.cross_links.len() {
        let _ = writeln!(
            c,
            "\n*Showing {} of {} — the full report is `roteiro links --matrix`.*",
            ws.cross_links.len(),
            ws.cross_links_total
        );
    }
    c.push_str(
        "\n*Shown in one direction only. The edge lives in the spoke's store, \
         pointing at a local placeholder for the hub's node, so the hub's own note \
         carries no matching **Incoming** entry — Obsidian's **Backlinks** pane \
         still shows it, because the link is in the vault.*\n",
    );
}

#[cfg(test)]
mod tests {
    use super::{
        AdrEntry, ConfigSecretSummary, CouplingEntry, CrossLink, DensityEntry, HOME_NOTE,
        VaultScope, VaultSummary, WorkspaceSummary, note_name, render_home, render_note,
        render_note_scoped, render_workspace_home, scoped_note_name,
    };
    use rto_graph::{EdgeRef, Explanation, NodeSummary};

    /// The shape of a name, pinned once so a change to it is a deliberate edit
    /// here rather than a diff spread over twenty other assertions.
    ///
    /// Everything else in this module composes `note_name` instead of repeating
    /// its output, because those tests are about *which key a link points at* and
    /// were never about the spelling.
    #[test]
    fn note_name_is_a_lowercase_hint_and_a_hash_of_the_whole_key() {
        assert_eq!(
            note_name("sym:rust:src/a.rs#Store"),
            "sym-rust-src-a.rs-store-b4cbf6633003361f"
        );
        assert_eq!(note_name("adr:0001"), "adr-0001-559a2e837953b2ff");
        assert_eq!(
            note_name("file:src/main.rs"),
            "file-src-main.rs-4a72627453f6780e"
        );
        // Deterministic: the suffix is a pure function of the key, so a vault
        // renders the same names on every machine and every run.
        assert_eq!(note_name("adr:0001"), note_name("adr:0001"));
    }

    /// **The property `note_name` exists to have** (issue #574): distinct keys
    /// give distinct notes *on a case-folding filesystem*, which is where the
    /// vault was losing them.
    ///
    /// Asserted over lowercased names, not names. On macOS and Windows two names
    /// differing only in case are one file, so a name set that is distinct as
    /// strings can still be a vault with notes missing — and Linux CI cannot see
    /// it. Folding here makes the assertion say what the filesystem says, on
    /// every platform.
    ///
    /// The keys are the two mechanisms that were actually losing notes, taken
    /// from this repository's own render rather than invented: the vendored
    /// `cytoscape.min.js` bundle whose minified single-letter symbols differ only
    /// by a sigil or by case, and a pair of grouped Rust `use` keys differing
    /// only by a trailing comma. `render_cli` runs the same assertion end to end
    /// over a rendered vault; this is the unit-level statement of it.
    #[test]
    fn distinct_keys_give_distinct_notes_even_after_case_folding() {
        const JS: &str = "sym:javascript:crates/roteiro/src/assets/cytoscape.min.js";
        let keys: Vec<String> = [
            // Slug lossiness: the sigil and the letter both slugged to the same
            // thing (9 notes lost this way, on every platform).
            format!("{JS}#$a"),
            format!("{JS}#a"),
            format!("{JS}#$o"),
            format!("{JS}#o"),
            // Case folding: distinct names, one file (95 notes lost this way, and
            // only on macOS and Windows).
            format!("{JS}#A"),
            format!("{JS}#O"),
            format!("{JS}#S"),
            format!("{JS}#s"),
            // Real source symbols, same shape.
            "sym:rust:crates/rto-exec/src/sandbox_store.rs#Store".into(),
            "sym:rust:crates/rto-exec/src/sandbox_store.rs#store".into(),
            // A trailing comma is the whole difference between these two.
            "import:rust:crate::engine::{ChatRequest,Engine,ModelInfo,}".into(),
            "import:rust:crate::engine::{ChatRequest,Engine,ModelInfo}".into(),
            // Nothing but separators: no hint at all, so the name is bare hash.
            "::".into(),
            "##".into(),
            // Over the length bound, differing only past the truncation point —
            // the case truncation alone used to merge.
            format!("import:rust:{}A", "a::b::c,".repeat(60)),
            format!("import:rust:{}a", "a::b::c,".repeat(60)),
        ]
        .into();

        let folded: std::collections::BTreeSet<String> =
            keys.iter().map(|k| note_name(k).to_lowercase()).collect();
        assert_eq!(
            folded.len(),
            keys.len(),
            "two keys share a note after case folding; the vault would hold one \
             file for both and report two"
        );
    }

    /// Case folding is the identity on a note name, so the assertion above is not
    /// weaker than the filesystem it stands in for.
    ///
    /// This is the reason the hint is lowercased rather than case-preserved: it
    /// makes "distinct names" and "distinct files on macOS" the same statement,
    /// so there is no version of this module that passes on Linux and loses notes
    /// on a Mac. Without it, the two assertions could drift apart and only the
    /// weaker one would ever run in CI.
    #[test]
    fn a_note_name_is_already_lowercase() {
        for key in [
            "sym:rust:src/a.rs#Store",
            "file:README.md",
            "app::file:CHANGELOG.md",
            "sym:javascript:a.js#ABC",
        ] {
            let name = note_name(key);
            assert_eq!(name, name.to_lowercase(), "`{key}` kept case in its name");
        }
    }

    /// `_Home` is a name in the same namespace as every note, and it is not
    /// derived from a key — so nothing must be able to collide with it. The
    /// mandatory suffix gives that for free: every generated name either ends in
    /// `-<16 hex>` or *is* 16 hex digits, and `_home` is neither.
    #[test]
    fn no_key_can_claim_the_home_note() {
        for key in ["_Home", "file:_Home", "_home", "::_Home::"] {
            assert_ne!(
                format!("{}.md", note_name(key)).to_lowercase(),
                HOME_NOTE.to_lowercase(),
                "`{key}` would overwrite the overview note"
            );
        }
    }

    #[test]
    fn render_note_emits_frontmatter_and_wikilinks() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "sym:rust:a.rs#main".into(),
                kind: "fn".into(),
                name: "main".into(),
                path: Some("a.rs".into()),
                lang: Some("rust".into()),
            },
            meta: serde_json::Value::Null,
            outgoing: vec![EdgeRef {
                kind: "calls".into(),
                provenance: "derived",
                confidence: None,
                node: "sym:rust:a.rs#helper".into(),
            }],
            incoming: vec![EdgeRef {
                kind: "references".into(),
                provenance: "authored",
                confidence: None,
                node: "adr:0001".into(),
            }],
        };
        let note = render_note(&ex, None, None);
        assert_eq!(
            note.filename,
            format!("{}.md", note_name("sym:rust:a.rs#main"))
        );
        assert!(note.content.contains("kind: fn"));
        // No source base → no Source link.
        assert!(!note.content.contains("**Source:**"));
        assert!(note.content.contains("# main"));
        assert!(note.content.contains(&format!(
            "- calls (derived) → [[{}]]",
            note_name("sym:rust:a.rs#helper")
        )));
        assert!(note.content.contains(&format!(
            "- [[{}]] references (authored) →",
            note_name("adr:0001")
        )));
        // Tags for the graph view.
        assert!(note.content.contains("- roteiro/kind/fn"));
        assert!(note.content.contains("- roteiro/lang/rust"));
    }

    #[test]
    fn note_name_bounds_long_keys_deterministically() {
        let long = format!("import:rust:{}", "a::b::c,".repeat(60));
        let a = note_name(&long);
        let b = note_name(&long);
        assert_eq!(a, b, "deterministic");
        assert!(
            a.len() <= 205,
            "bounded under the filename limit: {}",
            a.len()
        );
        assert_ne!(
            note_name(&format!("{long}x")),
            a,
            "different keys stay distinct after truncation"
        );
        // Truncation must not leave a doubled separator before the suffix — the
        // hint is trimmed after cutting, not before.
        assert!(!a.contains("--"), "{a}");
    }

    /// A short key is bounded too, and every name carries the suffix — the hash
    /// is no longer reached for only when the hint overruns.
    ///
    /// That gating was the defect (#574): two keys short enough to skip the hash
    /// had nothing left to tell them apart once the slug had flattened them.
    #[test]
    fn every_name_carries_the_hash_however_short_the_key() {
        for key in ["a", "adr:0001", "file:README.md"] {
            let name = note_name(key);
            let (hint, hash) = name.rsplit_once('-').expect("a suffixed name");
            assert!(!hint.is_empty(), "{name}");
            assert_eq!(hash.len(), 16, "{name}");
            assert!(
                hash.chars().all(|c| c.is_ascii_hexdigit()),
                "the suffix is the key's hash, not part of the hint: {name}"
            );
        }
        // A key with no hint at all is the bare hash, which cannot be mistaken
        // for a hinted name (those are at least 18 characters).
        let bare = note_name("::");
        assert_eq!(bare.len(), 16, "{bare}");
        assert!(!bare.contains('-'), "{bare}");
    }

    #[test]
    fn render_note_surfaces_content_and_status() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "adr:0001".into(),
                kind: "adr".into(),
                name: "Build Roteiro".into(),
                path: Some("docs/adr/0001.md".into()),
                lang: None,
            },
            meta: serde_json::json!({ "status": "Accepted", "content": "The decision text." }),
            outgoing: vec![],
            incoming: vec![],
        };
        let note = render_note(&ex, Some("https://github.com/org/repo/blob/abc123"), None);
        assert!(note.content.contains("status: Accepted"));
        assert!(note.content.contains("- roteiro/status/accepted"));
        assert!(note.content.contains("> **Status:** Accepted"));
        assert!(note.content.contains("## Content\n\nThe decision text."));
        // A clickable link to the actual ADR file on the repository host.
        assert!(
            note.content.contains(
                "**Source:** [`docs/adr/0001.md`](https://github.com/org/repo/blob/abc123/docs/adr/0001.md)"
            ),
            "{}",
            note.content
        );
    }

    /// The structured document a prose note is supposed to reproduce: headings, a
    /// table and a fenced code block, none of which survive whitespace collapse.
    const DOC: &str = "# Working offline\n\nRoteiro is **offline-capable**.\n\n| Host | What |\n| --- | --- |\n| `example.com` | models |\n\n```sh\nroteiro model pull\n```\n";

    fn prose_note(content: Option<&str>) -> Explanation {
        Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "file:docs/OFFLINE_SETUP.md".into(),
                kind: "file".into(),
                name: "OFFLINE_SETUP.md".into(),
                path: Some("docs/OFFLINE_SETUP.md".into()),
                lang: None,
            },
            meta: content.map_or(
                serde_json::Value::Null,
                |c| serde_json::json!({ "content": c }),
            ),
            outgoing: vec![],
            incoming: vec![],
        }
    }

    /// The whole readability defect, in one assertion pair: a note built from
    /// `meta.content` alone is the document whitespace-collapsed onto one line,
    /// and a note built from the source is the document.
    ///
    /// The newline count is the claim. A character count alone would pass on a
    /// note that had merely grown longer while staying flat, which is exactly the
    /// failure being fixed — `meta.content` is capped *and* collapsed, and only
    /// the collapse is what makes it unreadable.
    #[test]
    fn a_supplied_body_supersedes_the_collapsed_stored_content() {
        // What extraction stores: the same text, whitespace-collapsed.
        let collapsed = DOC.split_whitespace().collect::<Vec<_>>().join(" ");
        let ex = prose_note(Some(&collapsed));

        let note = render_note(&ex, None, Some(DOC));
        assert!(
            note.content.contains(DOC.trim()),
            "the source document is reproduced verbatim: {}",
            note.content
        );
        assert!(
            !note.content.contains(&collapsed),
            "the collapsed rendering is replaced, not appended: {}",
            note.content
        );
        assert!(
            note.content.contains("\n| Host | What |\n"),
            "a table needs its own lines to be a table: {}",
            note.content
        );
        assert!(
            note.content.contains("\n```sh\n"),
            "a fenced block needs its own lines to be a fence: {}",
            note.content
        );

        // The flat control: the same node with no body is the one-line note.
        let flat = render_note(&ex, None, None);
        assert!(
            flat.content.contains(&collapsed),
            "without a body the stored content is still shown: {}",
            flat.content
        );
        assert!(
            content_lines(&note.content) > content_lines(&flat.content),
            "structure restored: {} line(s) with a body vs {} without",
            content_lines(&note.content),
            content_lines(&flat.content)
        );
        assert_eq!(
            content_lines(&flat.content),
            1,
            "the defect: the stored content is a single line"
        );
    }

    /// A doc comment is a summary of a definition, not a document, and its note is
    /// correct as it stands. The caller supplies no body for these, so this pins
    /// the unchanged path — the fix must not depend on every node gaining one.
    #[test]
    fn a_note_with_no_body_is_unchanged() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "sym:rust:a.rs#main".into(),
                kind: "fn".into(),
                name: "main".into(),
                path: Some("a.rs".into()),
                lang: Some("rust".into()),
            },
            meta: serde_json::json!({ "content": "Entry point." }),
            outgoing: vec![],
            incoming: vec![],
        };
        assert!(
            render_note(&ex, None, None)
                .content
                .contains("## Content\n\nEntry point.")
        );
    }

    /// Lines in the note's `## Content` section.
    fn content_lines(note: &str) -> usize {
        let body = note
            .split_once("## Content\n\n")
            .map_or("", |(_, rest)| rest);
        let body = body.split_once("\n## ").map_or(body, |(head, _)| head);
        body.trim_end().lines().count()
    }

    #[test]
    fn render_note_shows_inferred_confidence() {
        let ex = Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: "file:a.md".into(),
                kind: "file".into(),
                name: "a.md".into(),
                path: Some("a.md".into()),
                lang: None,
            },
            meta: serde_json::Value::Null,
            outgoing: vec![EdgeRef {
                kind: "related".into(),
                provenance: "inferred",
                confidence: Some(0.82),
                node: "file:b.md".into(),
            }],
            incoming: vec![],
        };
        let note = render_note(&ex, None, None);
        assert!(
            note.content.contains(&format!(
                "related (inferred) (0.82) → [[{}]]",
                note_name("file:b.md")
            )),
            "{}",
            note.content
        );
    }

    #[test]
    fn render_home_summarises_the_graph() {
        let summary = VaultSummary {
            project: "demo".into(),
            total_nodes: 3,
            total_edges: 2,
            node_counts: vec![("fn".into(), 2), ("adr".into(), 1)],
            edge_provenance: vec![("derived".into(), 1), ("authored".into(), 1)],
            adrs: vec![AdrEntry {
                key: "adr:0001".into(),
                name: "First".into(),
                status: Some("Accepted".into()),
            }],
            debt: vec![("todo".into(), 4)], // roteiro:ignore
            densest_files: vec![DensityEntry {
                path: "src/small.rs".into(),
                markers: 3,
                lines: 120,
                per_kloc: 25.0,
            }],
            config_secrets: Some(ConfigSecretSummary {
                secret_named: 4,
                redacted: 3,
                declared: 1,
                unredacted: 0,
                files: vec![".env".into()],
            }),
            most_called: vec![CouplingEntry {
                key: "sym:rust:a.rs#helper".into(),
                name: "helper".into(),
                fan_in: 7,
                fan_out: 1,
            }],
            repo_url: Some("https://github.com/org/repo".into()),
            commit: Some("abcdef0123456789".into()),
        };
        let note = render_home(&summary);
        assert_eq!(note.filename, HOME_NOTE);
        assert!(note.content.contains("# demo — knowledge graph"));
        assert!(note.content.contains("**3 nodes**, **2 edges**"));
        assert!(note.content.contains("| fn | 2 |"));
        assert!(note.content.contains("| derived | 1 |"));
        assert!(note.content.contains(&format!(
            "**Accepted** — [[{}|First]]",
            note_name("adr:0001")
        )));
        assert!(note.content.contains("| todo | 4 |")); // roteiro:ignore
        // Directed coupling: the two fans are separate columns, and the wikilink's
        // own `|` is escaped so it cannot break the table it sits in.
        assert!(
            note.content.contains(&format!(
                "| [[{}\\|helper]] | 7 | 1 |",
                note_name("sym:rust:a.rs#helper")
            )),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("resolved by simple name"),
            "the precision caveat travels with the figures"
        );
        // Density: the count and the denominator are both shown, so the ratio can
        // be checked rather than taken on trust, and the wikilink's own `|` is
        // escaped so it cannot break the table it sits in.
        assert!(
            note.content.contains(&format!(
                "| [[{}\\|src/small.rs]] | 3 | 120 | 25.00 |",
                note_name("file:src/small.rs")
            )),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("not source lines of code"),
            "the denominator caveat travels with the figures"
        );
        // Config secrets: counts and files, and no key names — a vault note is
        // browsed out of context, which is the wrong place for a list that would
        // read as a secret scan's output.
        assert!(
            note.content.contains(
                "**4** secret-named config key(s): 3 redacted before storage, 1 \
                 declared in code without a value, 0 unredacted."
            ),
            "{}",
            note.content
        );
        assert!(
            note.content
                .contains(&format!("- [[{}\\|.env]]", note_name("file:.env"))),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("not a secret scan")
                && note.content.contains("cannot see a hardcoded credential"),
            "the limitation travels with the figures: {}",
            note.content
        );
        assert!(
            !note.content.contains("[!warning]"),
            "no warning when nothing is unredacted: {}",
            note.content
        );
        // A repository link + short-commit permalink note.
        assert!(
            note.content
                .contains("**Repository:** [https://github.com/org/repo](https://github.com/org/repo) · rendered at commit `abcdef012345`"),
            "{}",
            note.content
        );
    }

    #[test]
    fn render_home_omits_density_for_a_graph_with_no_markers() {
        // A clean repository has no markers, so there is no density to rank. An
        // empty table under a heading reads as "measured, and there is nothing";
        // the section is absent instead. Same rule as the coupling table below.
        let note = render_home(&VaultSummary {
            project: "clean".into(),
            total_nodes: 1,
            ..VaultSummary::default()
        });
        assert!(
            !note.content.contains("Densest files"),
            "no heading without rows: {}",
            note.content
        );
        // The intent-debt section itself still renders — density is an addition
        // to it, not a replacement.
        assert!(note.content.contains("## Intent debt"));
        assert!(note.content.contains("*None recorded.*"));
    }

    #[test]
    fn render_home_omits_config_secrets_rather_than_rendering_zeroes() {
        // A row of zeroes under this heading would read as "scanned, and clean" —
        // a conclusion the lens cannot support, since a credential under an
        // innocuous key name never appears in it. The section is absent instead.
        let note = render_home(&VaultSummary {
            project: "clean".into(),
            total_nodes: 1,
            ..VaultSummary::default()
        });
        assert!(
            !note.content.contains("named like secrets"),
            "no heading without figures: {}",
            note.content
        );
    }

    #[test]
    fn render_home_warns_loudly_about_an_unredacted_value() {
        // Extraction cannot produce this state, so if it appears something else
        // put an unredacted value in the store — and the note must say where to
        // look rather than implicating the repository.
        let note = render_home(&VaultSummary {
            project: "imported".into(),
            total_nodes: 1,
            config_secrets: Some(ConfigSecretSummary {
                secret_named: 1,
                redacted: 0,
                declared: 0,
                unredacted: 1,
                files: vec!["imported.env".into()],
            }),
            ..VaultSummary::default()
        });
        assert!(
            note.content.contains("[!warning]") && note.content.contains("**unredacted**"),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("came from an import layer"),
            "and it points at the importing tool, not the repository: {}",
            note.content
        );
    }

    #[test]
    fn render_home_omits_coupling_for_a_graph_with_no_calls() {
        // A prose-only vault has no `calls` edges. An empty table under a heading
        // reads as "measured, and there is nothing" — the section is absent instead.
        let note = render_home(&VaultSummary {
            project: "docs".into(),
            total_nodes: 1,
            ..VaultSummary::default()
        });
        assert!(
            !note.content.contains("Most depended-on"),
            "no heading without rows: {}",
            note.content
        );
        // The rest of the overview is unaffected.
        assert!(note.content.contains("# docs — knowledge graph"));
    }

    // ---- Workspace vaults (issue #442 part 1) --------------------------------

    /// A `Explanation` for `key`, with one outgoing edge to `to`.
    fn node_linking_to(key: &str, name: &str, to: &str) -> Explanation {
        Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: key.into(),
                kind: "config_key".into(),
                name: name.into(),
                path: Some("config.toml".into()),
                lang: None,
            },
            meta: serde_json::Value::Null,
            outgoing: vec![EdgeRef {
                kind: "links".into(),
                provenance: "inferred",
                confidence: Some(0.91),
                node: to.into(),
            }],
            incoming: vec![],
        }
    }

    fn members(names: &[&str]) -> std::collections::BTreeSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    /// **Rewritten deliberately under #574.** #570 landed this as "a project
    /// scope leaves every note name exactly as it was", and read that two ways at
    /// once: `PROJECT` reduces to `note_name`, *and* `note_name` itself does not
    /// move. #574 breaks the second half on purpose — the old names were not
    /// injective under filename case folding and this repository's vault lost 104
    /// notes to it — so the two halves are separated here rather than having
    /// expected values quietly updated underneath the old title.
    ///
    /// What survives is the half #570 was actually about, and it is unweakened:
    /// **turning workspace mode on must not rename a project's notes.** Names may
    /// move when `note_name` changes, for a reason argued at `note_name`; they may
    /// never move because a repository happens to sit inside a configured
    /// workspace, because that would happen by inference rather than by a release.
    ///
    /// The other half of #570's promise — that a project render is byte-identical
    /// apart from names — is now [`render_note_is_the_project_scoped_render_byte_for_byte`]
    /// and `render_cli`'s end-to-end pair.
    #[test]
    fn a_project_scope_never_qualifies_a_name() {
        // A user's own notes live outside the vault and link into it *by name*
        // (#442), so a rename breaks them silently, with no error and nothing to
        // grep for. Whatever workspace mode does, `VaultScope::PROJECT` must
        // reduce to `note_name` of the bare key.
        for key in [
            "file:README.md",
            "adr:0001",
            "sym:rust:src/a.rs#Store",
            "extref:other::file:README.md",
            "cfgkey:config.toml#serve.addr",
        ] {
            assert_eq!(
                scoped_note_name(&VaultScope::PROJECT, key),
                note_name(key),
                "single-project name moved for `{key}`"
            );
            // And the qualified form really is a different name, so the assertion
            // above is not vacuously true of every scope.
            let ms = members(&["app"]);
            assert_ne!(
                scoped_note_name(
                    &VaultScope {
                        project: Some("app"),
                        members: &ms,
                    },
                    key
                ),
                note_name(key),
                "qualification must move the name for `{key}`, or nothing above holds"
            );
        }
    }

    #[test]
    fn render_note_is_the_project_scoped_render_byte_for_byte() {
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", "sym:rust:a.rs#A");
        assert_eq!(
            render_note(&ex, Some("https://h/b"), Some("body")),
            render_note_scoped(&ex, Some("https://h/b"), Some("body"), &VaultScope::PROJECT),
            "the unscoped entry point must stay the scoped one at PROJECT, so the \
             two cannot drift apart"
        );
    }

    #[test]
    fn each_member_gets_its_own_note_for_the_same_key() {
        // The collision the whole feature exists for: node keys are
        // repository-relative, so every member's `README.md` is `file:README.md`.
        let ms = members(&["api", "sdk"]);
        let names: Vec<String> = ["api", "sdk"]
            .iter()
            .map(|p| {
                scoped_note_name(
                    &VaultScope {
                        project: Some(p),
                        members: &ms,
                    },
                    "file:README.md",
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                note_name("api::file:README.md"),
                note_name("sdk::file:README.md")
            ]
        );
        assert_ne!(names[0], names[1], "two members must not share one note");
    }

    /// The two names this feature has, pinned together in one place.
    ///
    /// They are easy to conflate and were, in this PR, described inconsistently
    /// in two doc comments — the **key** is `<project>::<key>` (ADR-0009's
    /// cross-repo form, which is why cross-repo links resolve), and the **note
    /// name** is [`note_name`] of that key, in which `::` has become `-`. A
    /// reader told the wrong one goes looking for a file with `::` in it.
    ///
    /// Asserting both here means the next description that drifts has something
    /// to disagree with, rather than waiting for a reviewer to read two comments
    /// side by side.
    #[test]
    fn the_qualified_key_and_the_note_name_are_different_strings() {
        let ms = members(&["app"]);
        let scope = VaultScope {
            project: Some("app"),
            members: &ms,
        };
        // The key: project-qualified, `::` intact — this is what the graph and
        // ADR-0009's external refs use.
        let qualified = "app::file:README.md";
        // The note name: `note_name` of exactly that key, `::` slugged to `-`,
        // the whole hint lowercased, and the key's own hash appended.
        assert_eq!(
            scoped_note_name(&scope, "file:README.md"),
            "app-file-readme.md-a114bde6dcaba1c1"
        );
        assert_eq!(note_name(qualified), "app-file-readme.md-a114bde6dcaba1c1");
        assert!(
            !scoped_note_name(&scope, "file:README.md").contains("::"),
            "no note name ever contains `::`"
        );
        // And on disk the stem gains the extension, which is the string a reader
        // actually looks for.
        let note = render_note_scoped(
            &node_with("file:README.md", Some("README.md"), None),
            None,
            None,
            &scope,
        );
        assert_eq!(note.filename, "app-file-readme.md-a114bde6dcaba1c1.md");
    }

    /// `_Home` must *show* a name, not spell the form out.
    ///
    /// The test above pins the distinction in the code. It did not stop the
    /// distinction being described wrongly in the same file, because it guards
    /// the function and not the sentences: `render_workspace_home` went on
    /// writing the pre-#574 form into the `_Home` of every workspace vault
    /// v2.0.0 built, and nothing here disagreed with it.
    ///
    /// So this asserts the property that made that possible is gone — the
    /// paragraph now contains a string `note_name` actually produced for a key
    /// the workspace really holds, which a hand-written spelling cannot
    /// satisfy. It is not a tautology despite both sides calling `note_name`:
    /// what it rejects is the *shape* of the old copy, a form written out by
    /// hand next to the function that could have rendered it.
    ///
    /// That the *key* is real is the other half, and the reason the example is
    /// drawn from `cross_links` rather than invented from the member list —
    /// a name rendered for a node the vault does not hold is a true sentence
    /// about a note nobody can open. The empty case is
    /// `the_workspace_home_claims_no_example_note_when_it_has_no_real_key`.
    /// #573: the cross-repo section distinguishes a **declaration** from a
    /// **match**, and says how many of each.
    ///
    /// Before authored links could be persisted, every row was a candidate and
    /// the section said so in one blanket caveat. That caveat is now false for
    /// declared rows, and an edge that exists but renders as a guess leaves
    /// ADR-0009's `authored → gold` path just as unreachable as no edge at all —
    /// so the rendering is part of the contract, not decoration.
    #[test]
    fn the_cross_repo_section_separates_declared_links_from_inferred_ones() {
        let link = |authored: bool, key: &str| CrossLink {
            from_project: "sdk".into(),
            from_key: format!("cfgkey:config.toml#{key}"),
            from_name: key.into(),
            kind: "references".into(),
            // A declaration carries no score by construction; a match does.
            confidence: if authored { None } else { Some(0.91) },
            to_qualified: format!("api::cfgkey:config.toml#{key}"),
            resolves: true,
            authored,
        };
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![link(true, "addr"), link(false, "port")],
            cross_links_total: 2,
        };
        let c = render_workspace_home(&ws).content;

        assert!(c.contains("**1 declared**"), "{c}");
        assert!(c.contains("**1 inferred**"), "{c}");
        assert!(
            !c.contains("not authored facts"),
            "the blanket caveat is false once a declared row can appear: {c}"
        );
        // Per row: a declaration is marked as one, and a match keeps its score.
        assert!(c.contains("references *(declared)*"), "{c}");
        assert!(c.contains("references (0.91)"), "{c}");
    }

    #[test]
    fn the_workspace_home_names_an_example_note_name_actually_produces() {
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![CrossLink {
                from_project: "sdk".into(),
                from_key: "cfgkey:config.toml#addr".into(),
                from_name: "addr".into(),
                kind: "links".into(),
                confidence: Some(0.91),
                to_qualified: "api::cfgkey:config.toml#addr".into(),
                resolves: true,
                authored: false,
            }],
            cross_links_total: 1,
        };
        let note = render_workspace_home(&ws);

        // The source end of the first cross-repo link, rendered through the real
        // function. `from_project` is a member and `from_key` is one of its own
        // nodes, so this is a note the render writes rather than one the
        // sentence assumes.
        let expected = format!("{}.md", note_name("sdk::cfgkey:config.toml#addr"));
        assert!(
            note.content.contains(&expected),
            "the naming paragraph must show a real name ({expected}), not a \
             hand-written form:\n{}",
            note.content
        );
        // And the key form it is derived *from* is still stated, because that is
        // the half a reader needs to look a note up by its frontmatter.
        assert!(
            note.content.contains("`<project>::<key>`"),
            "{}",
            note.content
        );
        // No filename anywhere in the vault carries `::`.
        assert!(!expected.contains("::"), "{expected}");
    }

    /// With no cross-repo links there is no key the renderer can prove is a
    /// node, so it must say nothing rather than fabricate one.
    ///
    /// The example this replaced was `<first member>::file:README.md`, invented
    /// from the member list — and membership does not require a README, so
    /// `_Home` could assert a note that was never written. That is the very
    /// defect this PR exists to fix, one remove away, so the empty case gets an
    /// assertion of its own rather than an assumption.
    #[test]
    fn the_workspace_home_claims_no_example_note_when_it_has_no_real_key() {
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![],
            cross_links_total: 0,
        };
        let note = render_workspace_home(&ws);

        assert!(
            !note.content.contains("is the note "),
            "no cross-repo link means no provable key, so no `Here, X is the \
             note Y` claim:\n{}",
            note.content
        );
        // The fabricated form specifically: never emitted, with or without links.
        assert!(
            !note.content.contains("::file:README.md"),
            "{}",
            note.content
        );
        // The rule itself is still stated — only the illustration is absent.
        assert!(
            note.content.contains("`<project>::<key>`")
                && note.content.contains("no filename contains `::`"),
            "{}",
            note.content
        );
    }

    #[test]
    fn a_member_note_declares_which_member_it_came_from() {
        let ms = members(&["api"]);
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", "sym:rust:a.rs#A");
        let note = render_note_scoped(
            &ex,
            None,
            None,
            &VaultScope {
                project: Some("api"),
                members: &ms,
            },
        );
        assert_eq!(
            note.filename,
            format!("{}.md", note_name("api::cfgkey:config.toml#addr"))
        );
        assert!(
            note.content.contains("project: \"api\""),
            "{}",
            note.content
        );
        assert!(
            note.content.contains("- roteiro/project/api"),
            "the tag is what filters the graph view to one repository: {}",
            note.content
        );
        // A within-member edge is qualified to the same member, not left bare.
        assert!(
            note.content
                .contains(&format!("→ [[{}]]", note_name("api::sym:rust:a.rs#A"))),
            "{}",
            note.content
        );
    }

    #[test]
    fn a_project_note_declares_no_project() {
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", "sym:rust:a.rs#A");
        let note = render_note(&ex, None, None);
        assert!(!note.content.contains("project:"), "{}", note.content);
        assert!(
            !note.content.contains("roteiro/project/"),
            "a per-project vault would carry one constant on every note — and \
             adding it would change every note's bytes: {}",
            note.content
        );
    }

    #[test]
    fn a_cross_repo_edge_links_straight_to_the_other_members_note() {
        // ADR-0009: the spoke's edge points at a *local placeholder* for the hub's
        // node, because store integrity needs both ends in one store. A workspace
        // vault holds both, so the link goes to the real note. No new edge — the
        // resolver already follows this placeholder at query time.
        let ms = members(&["spoke", "hub"]);
        let scope = VaultScope {
            project: Some("spoke"),
            members: &ms,
        };
        let ex = node_linking_to(
            "cfgkey:config.toml#addr",
            "addr",
            &rto_graph::external_ref_key("hub::cfgkey:config.toml#addr"),
        );
        let note = render_note_scoped(&ex, None, None, &scope);
        assert!(
            note.content.contains(&format!(
                "→ [[{}]]",
                note_name("hub::cfgkey:config.toml#addr")
            )),
            "the edge must land on the hub's own note: {}",
            note.content
        );
        assert!(
            !note.content.contains("extref"),
            "and never on the placeholder: {}",
            note.content
        );
        // The same rule decides that the placeholder is not written as a note, so
        // the two halves cannot disagree.
        assert!(
            scope.redirects_external_ref(&rto_graph::external_ref_key(
                "hub::cfgkey:config.toml#addr"
            ))
        );
    }

    #[test]
    fn a_cross_repo_edge_out_of_the_workspace_keeps_its_placeholder() {
        // The target repo is not in this vault, so there is no note to point at.
        // Redirecting anyway would produce a link that resolves to nothing —
        // Obsidian shows that as merely unwritten, which is a worse lie than a
        // placeholder that honestly says "elsewhere".
        let ms = members(&["spoke"]);
        let scope = VaultScope {
            project: Some("spoke"),
            members: &ms,
        };
        let key = rto_graph::external_ref_key("elsewhere::cfgkey:config.toml#addr");
        assert!(!scope.redirects_external_ref(&key));
        let ex = node_linking_to("cfgkey:config.toml#addr", "addr", &key);
        let note = render_note_scoped(&ex, None, None, &scope);
        assert!(
            note.content.contains(&format!(
                "→ [[{}]]",
                note_name("spoke::extref:elsewhere::cfgkey:config.toml#addr")
            )),
            "{}",
            note.content
        );
    }

    #[test]
    fn a_single_project_vault_never_redirects_an_external_ref() {
        // No members ⇒ nothing to resolve against, so today's vault keeps rendering
        // the placeholder exactly as it does now.
        let key = rto_graph::external_ref_key("hub::cfgkey:config.toml#addr");
        assert!(!VaultScope::PROJECT.redirects_external_ref(&key));
        assert_eq!(
            scoped_note_name(&VaultScope::PROJECT, &key),
            note_name(&key)
        );
    }

    fn member_summary(project: &str, fan_in: u32) -> VaultSummary {
        VaultSummary {
            project: project.to_owned(),
            total_nodes: 3,
            total_edges: 2,
            node_counts: vec![("fn".into(), 2)],
            edge_provenance: vec![("derived".into(), 2)],
            adrs: vec![AdrEntry {
                key: "adr:0001".into(),
                name: "First".into(),
                status: Some("Accepted".into()),
            }],
            debt: vec![("todo".into(), 4)], // roteiro:ignore
            densest_files: vec![DensityEntry {
                path: "src/small.rs".into(),
                markers: 3,
                lines: 120,
                per_kloc: 25.0,
            }],
            config_secrets: None,
            most_called: vec![CouplingEntry {
                key: "sym:rust:a.rs#helper".into(),
                name: "helper".into(),
                fan_in,
                fan_out: 1,
            }],
            repo_url: Some(format!("https://github.com/org/{project}")),
            commit: Some("abcdef0123456789".into()),
        }
    }

    #[test]
    fn the_workspace_home_keeps_every_members_own_aggregates() {
        // The promise in issue #442: the existing per-project `_Home` view is a
        // *subset* of the workspace one, not a casualty of it. Someone who came for
        // their repository's coupling and debt tables must still find them —
        // not a workspace total that averages them away.
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![],
            cross_links_total: 0,
        };
        let note = render_workspace_home(&ws);
        assert_eq!(note.filename, HOME_NOTE);
        assert!(
            note.content
                .contains("# platform — workspace knowledge graph")
        );
        // Summed, and the members listed.
        assert!(
            note.content
                .contains("**6 nodes**, **4 edges** across **2** member")
        );
        assert!(note.content.contains("| [[#api\\|api]] | 3 | 2 |"));

        for project in ["api", "sdk"] {
            assert!(
                note.content.contains(&format!("\n## {project}\n")),
                "each member gets its own section"
            );
        }
        // Today's sections, one level deeper, once per member.
        for section in [
            "### Structure",
            "### Provenance",
            "### Decisions (ADRs)",
            "### Intent debt",
            "#### Densest files",
            "### Most depended-on",
        ] {
            assert_eq!(
                note.content.matches(section).count(),
                2,
                "`{section}` must appear once per member: {}",
                note.content
            );
        }
        // And every link inside a member's section resolves within that member.
        assert!(note.content.contains(&format!(
            "**Accepted** — [[{}|First]]",
            note_name("api::adr:0001")
        )));
        assert!(note.content.contains(&format!(
            "**Accepted** — [[{}|First]]",
            note_name("sdk::adr:0001")
        )));
        assert!(note.content.contains(&format!(
            "[[{}\\|helper]] | 7 |",
            note_name("api::sym:rust:a.rs#helper")
        )));
        assert!(note.content.contains(&format!(
            "[[{}\\|src/small.rs]]",
            note_name("sdk::file:src/small.rs")
        )));
    }

    #[test]
    fn the_workspace_home_renders_cross_repo_links_and_marks_the_ones_it_cannot_follow() {
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7), member_summary("sdk", 4)],
            cross_links: vec![
                CrossLink {
                    from_project: "sdk".into(),
                    from_key: "cfgkey:config.toml#addr".into(),
                    from_name: "addr".into(),
                    kind: "links".into(),
                    confidence: Some(0.91),
                    to_qualified: "api::cfgkey:config.toml#addr".into(),
                    resolves: true,
                    authored: false,
                },
                CrossLink {
                    from_project: "sdk".into(),
                    from_key: "cfgkey:config.toml#other".into(),
                    from_name: "other".into(),
                    kind: "links".into(),
                    confidence: None,
                    to_qualified: "absent::cfgkey:config.toml#other".into(),
                    resolves: false,
                    authored: false,
                },
            ],
            cross_links_total: 2,
        };
        let note = render_workspace_home(&ws);
        // Resolvable: a link to the other member's note, with its confidence.
        assert!(
            note.content.contains(&format!(
                "| [[{}\\|addr]] | sdk | [[{}\\|api::cfgkey:config.toml#addr]] | links (0.91) |",
                note_name("sdk::cfgkey:config.toml#addr"),
                note_name("api::cfgkey:config.toml#addr"),
            )),
            "{}",
            note.content
        );
        // Outside the workspace: stated as such, never as a wikilink — Obsidian
        // renders a link to a missing note as one that is merely unwritten.
        assert!(
            note.content
                .contains("`absent::cfgkey:config.toml#other` *(outside this workspace)*"),
            "{}",
            note.content
        );
        assert!(
            !note.content.contains("[[absent-"),
            "a dangling wikilink would read as a note someone forgot to write: {}",
            note.content
        );
    }

    #[test]
    fn the_workspace_home_says_when_it_has_truncated_the_cross_links() {
        // A capped table that does not say it is capped reads as the whole set.
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7)],
            cross_links: vec![CrossLink {
                from_project: "api".into(),
                from_key: "cfgkey:config.toml#addr".into(),
                from_name: "addr".into(),
                kind: "links".into(),
                confidence: None,
                to_qualified: "api::cfgkey:config.toml#addr".into(),
                resolves: true,
                authored: false,
            }],
            cross_links_total: 40,
        };
        let note = render_workspace_home(&ws);
        assert!(note.content.contains("Showing 1 of 40"), "{}", note.content);
        assert!(note.content.contains("roteiro links --matrix"));
    }

    #[test]
    fn a_workspace_with_no_cross_repo_links_says_why_rather_than_showing_nothing() {
        let ws = WorkspaceSummary {
            name: "platform".into(),
            members: vec![member_summary("api", 7)],
            cross_links: vec![],
            cross_links_total: 0,
        };
        let note = render_workspace_home(&ws);
        assert!(note.content.contains("## Cross-repo links"));
        assert!(
            note.content.contains("links --infer --write"),
            "an empty section must name what would fill it, or it reads as \
             \"these repos are unrelated\": {}",
            note.content
        );
        // Singular, because getting this wrong on a one-member workspace is the
        // kind of thing nobody notices until it ships.
        assert!(note.content.contains("**1** member repository."));
    }

    // ---- YAML frontmatter escaping -------------------------------------------

    /// Parse a note's frontmatter block with a **real** YAML parser and return
    /// `field`'s value, or the parse error.
    ///
    /// Every assertion below goes through this rather than checking the emitted
    /// bytes. An escaper that is wrong in a self-consistent way passes a
    /// byte-comparison — that is precisely how `"foo\bar"` survived: it looks
    /// exactly like what was asked for, and means something else.
    fn frontmatter_field(note: &str, field: &str) -> Result<Option<String>, String> {
        let block = note
            .strip_prefix("---\n")
            .and_then(|rest| rest.split_once("\n---\n"))
            .map(|(block, _)| block)
            .expect("note must open with a frontmatter block");
        let docs = yaml_rust2::YamlLoader::load_from_str(block).map_err(|e| e.to_string())?;
        Ok(docs[0][field].as_str().map(ToOwned::to_owned))
    }

    /// A node whose key, path and language are whatever the test needs.
    fn node_with(key: &str, path: Option<&str>, lang: Option<&str>) -> Explanation {
        Explanation {
            schema: rto_graph::SCHEMA,
            node: NodeSummary {
                key: key.into(),
                kind: "fn".into(),
                name: "n".into(),
                path: path.map(ToOwned::to_owned),
                lang: lang.map(ToOwned::to_owned),
            },
            meta: serde_json::Value::Null,
            outgoing: vec![],
            incoming: vec![],
        }
    }

    /// The three measured failure modes of the escaping this replaced, each
    /// asserted on the **parsed** value.
    ///
    /// Before the fix: `foo\bar` parsed back as `foo<BS>ar` (silently six
    /// characters, not seven), and the other two made the whole block
    /// unparseable — which in Obsidian costs the note *every* property, with no
    /// error shown.
    #[test]
    fn a_backslash_or_quote_in_a_path_still_parses_back_to_itself() {
        for path in [
            r"foo\bar",     // `\b` was YAML's backspace escape: silent corruption
            r"foo\dir",     // `\d` is not a YAML escape at all: parse error
            "say\"hi\".rs", // an unescaped `"` ended the scalar early: parse error
            r"a\\b",
            "trailing-backslash\\",
        ] {
            let note = render_note(&node_with("file:x", Some(path), None), None, None);
            assert_eq!(
                frontmatter_field(&note.content, "path"),
                Ok(Some(path.to_owned())),
                "path {path:?} must round-trip"
            );
        }
    }

    /// `key:` is not hypothetical for this: node keys already carry `:` and `#`,
    /// and a symbol name can contain a quotation mark.
    #[test]
    fn a_node_key_round_trips_whatever_punctuation_it_carries() {
        for key in [
            "sym:rust:src/a.rs#Store",
            r"sym:rust:src\weird.rs#Thing",
            "sym:rust:a.rs#say\"hi\"",
            "cfgkey:config.toml#serve.addr",
        ] {
            let note = render_note(&node_with(key, None, None), None, None);
            assert_eq!(
                frontmatter_field(&note.content, "key"),
                Ok(Some(key.to_owned())),
                "key {key:?} must round-trip"
            );
        }
        // The old rule turned a `"` into an apostrophe, so the note reported a key
        // that was not the node's key — parseable, and wrong.
        let note = render_note(
            &node_with("sym:rust:a.rs#say\"hi\"", None, None),
            None,
            None,
        );
        assert!(
            !note.content.contains("say'hi'"),
            "a quotation mark must be escaped, not rewritten: {}",
            note.content
        );
    }

    /// A member directory name is a path component, so it reaches the same rule.
    #[test]
    fn a_member_project_name_round_trips() {
        let ms: std::collections::BTreeSet<String> =
            std::iter::once(r"odd\name".to_owned()).collect();
        let note = render_note_scoped(
            &node_with("file:x", None, None),
            None,
            None,
            &VaultScope {
                project: Some(r"odd\name"),
                members: &ms,
            },
        );
        assert_eq!(
            frontmatter_field(&note.content, "project"),
            Ok(Some(r"odd\name".to_owned()))
        );
    }

    /// The **bare** fields are the other half of the same class, and were missed
    /// by the review that found the quoted ones: `status` is written unquoted, and
    /// `roteiro load` installs a caller-supplied artifact whose nodes carry
    /// whatever they carry.
    #[test]
    fn a_bare_field_is_quoted_only_when_being_bare_would_change_it() {
        let with_status = |status: &str| {
            let mut ex = node_with("adr:0001", None, None);
            ex.meta = serde_json::json!({ "status": status });
            render_note(&ex, None, None)
        };

        // Would be a parse error bare; would silently truncate bare.
        for status in [
            "Accepted: superseded by 0012",
            "Accepted # pending",
            "{draft}",
            "",
        ] {
            let note = with_status(status);
            assert_eq!(
                frontmatter_field(&note.content, "status"),
                Ok(Some(status.to_owned())),
                "status {status:?} must round-trip"
            );
        }

        // …and a safe one stays bare, which is what keeps an existing vault's
        // bytes unchanged.
        let note = with_status("Accepted");
        assert!(
            note.content.contains("\nstatus: Accepted\n"),
            "a plain-safe status must not gain quotes: {}",
            note.content
        );
    }

    /// `no` is Norwegian, and a bare `no` reads as `false` to a YAML **1.1**
    /// parser.
    ///
    /// The only assertion here that pins emitted bytes, and deliberately so:
    /// `yaml-rust2` implements YAML 1.2, whose core schema resolves a bare `no`
    /// to the *string* `no`, so a round-trip through this test's own oracle
    /// cannot see the problem — it passes either way. The exposure is to the
    /// parser on the other side, and Obsidian's is not this one. Quoting costs
    /// two characters on a value that never occurs here; guessing which YAML
    /// version every downstream reader implements does not seem like the better
    /// bet.
    #[test]
    fn a_language_that_spells_a_yaml_boolean_is_quoted() {
        let note = render_note(&node_with("file:x", None, Some("no")), None, None);
        assert!(
            note.content.contains("\nlang: \"no\"\n"),
            "a bare `no` is `false` to a 1.1 parser and must be quoted: {}",
            note.content
        );
        assert_eq!(
            frontmatter_field(&note.content, "lang"),
            Ok(Some("no".to_owned())),
            "and it must still read back as the string: {}",
            note.content
        );
        // And an ordinary language is untouched.
        let rust = render_note(&node_with("file:x", None, Some("rust")), None, None);
        assert!(rust.content.contains("\nlang: rust\n"), "{}", rust.content);
    }

    /// Control characters and the separators some parsers fold as line breaks.
    #[test]
    fn control_characters_cannot_break_out_of_the_block() {
        for path in [
            "a\nb",
            "a\tb",
            "a\u{0}b",
            "a\u{2028}b",
            "a\u{7f}b",
            "a\u{85}b",
        ] {
            let note = render_note(&node_with("file:x", Some(path), None), None, None);
            assert_eq!(
                frontmatter_field(&note.content, "path"),
                Ok(Some(path.to_owned())),
                "path {path:?} must round-trip"
            );
            // A raw newline would end the scalar and inject a sibling key.
            assert_eq!(
                note.content.matches("\npath: ").count(),
                1,
                "the value must stay on one line: {}",
                note.content
            );
        }
    }

    /// The escaping is *only* an escaping: for a value with nothing to escape it
    /// must emit the same bytes it always did, or #442's promise that a
    /// single-project vault is byte-identical does not hold.
    #[test]
    fn an_ordinary_value_is_emitted_exactly_as_before() {
        let note = render_note(
            &node_with("sym:rust:src/a.rs#Store", Some("src/a.rs"), Some("rust")),
            None,
            None,
        );
        assert!(
            note.content
                .contains("\nkey: \"sym:rust:src/a.rs#Store\"\n")
        );
        assert!(note.content.contains("\nkind: fn\n"));
        assert!(note.content.contains("\npath: \"src/a.rs\"\n"));
        assert!(note.content.contains("\nlang: rust\n"));
    }

    /// The plain-style decision is checked against a real parser rather than
    /// against itself: whatever `is_plain_safe` accepts must actually round-trip
    /// bare, and whatever it rejects must round-trip quoted.
    #[test]
    fn the_plain_style_decision_agrees_with_a_real_yaml_parser() {
        for value in [
            "fn",
            "config_key",
            "rust",
            "Accepted",
            "a.b",
            "a/b",
            "a-b_c",
            "no",
            "yes",
            "true",
            "null",
            "y",
            "N",
            "",
            " lead",
            "trail ",
            "a: b",
            "a #c",
            "{x}",
            "[x]",
            "*x",
            "&x",
            "!x",
            "#x",
            ">x",
            "|x",
            "%x",
            "@x",
            "`x",
            "\"x",
            "'x",
            ",x",
            "123",
            "1.5",
            "-x",
            ".x",
            "a\\b",
        ] {
            let emitted = super::yaml_scalar(value);
            let doc = format!("v: {emitted}");
            let parsed = yaml_rust2::YamlLoader::load_from_str(&doc)
                .unwrap_or_else(|e| panic!("{value:?} emitted {emitted:?}: {e}"));
            assert_eq!(
                parsed[0]["v"].as_str(),
                Some(value),
                "{value:?} emitted as {emitted:?} did not round-trip"
            );
        }
    }
}

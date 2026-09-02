//! Syntax checking for the fenced code blocks in an OKF bundle.
//!
//! This crate exists because of a separation-of-concerns problem in the OKF
//! ecosystem, and it is written to be given away.
//!
//! # Why it is separate
//!
//! Upstream's `okf-validator` answers two different questions with one crate:
//! *"is this a conformant OKF bundle?"* — frontmatter shape, trust tiers,
//! provenance, link resolution, concept-id rules — and *"does the Python in this
//! fenced block parse?"*. The first is what an interchange format is for. The
//! second is a linter's job, and every consumer of the format checker pays the
//! supply chain of the code checker to get it: `rustpython-parser` alone is 61
//! crates, carries `LGPL-3.0-only` through the `malachite` tree, and pulls six
//! unmaintained `unic-*` advisories whose own text says no safe upgrade exists.
//!
//! That bought two of the validator's thirty-four checks. Roteiro could not take
//! it — `cargo deny` refuses the tree on both licence and advisory grounds, and
//! ADR-0017 §3 forbids admitting a licence merely to turn CI green.
//!
//! # The shape: a pure-Rust core, and every parser optional
//!
//! The lesson of that failure is not "pick better parsers", it is that **a
//! consumer should choose what it is willing to compile**. So the core here
//! parses no programming language at all, and every backend — *including*
//! tree-sitter — is a feature.
//!
//! With `--no-default-features` this crate compiles no grammar and invokes no C
//! toolchain, and still does everything that needs no code parser:
//!
//! | Always available | How |
//! | --- | --- |
//! | Lifting fenced blocks out of markdown | [`extract_fenced_code_blocks`] |
//! | JSON | `serde_json` |
//! | YAML | `okf_core::yaml` — the parser that read the frontmatter |
//! | Shell quoting and bracket balance | a small matcher in this crate |
//!
//! | Feature | Adds | Costs |
//! | --- | --- | --- |
//! | `grammars` *(default)* | Python, JavaScript, TypeScript, Rust, Bash | tree-sitter grammars, which compile C |
//! | `sql` *(default)* | SQL, via `sqlparser` | ~17 crates, two of which compile assembly |
//!
//! That is also what makes the crate donatable. Upstream describes itself as a
//! pure-Rust implementation, so a backend that compiles C cannot be mandatory;
//! with this shape they can take the core, keep their existing parsers behind
//! their own features, and consumers who only want conformance opt out of all of
//! it. See `W4G1/okf#4`.
//!
//! # A checker that cannot check must say so
//!
//! Because backends are optional, a language can be *unsupported in this build*
//! rather than *clean*. Conflating those is how a check becomes vacuous — it
//! passes because nothing ran.
//!
//! [`check_syntax`] still returns `Ok` for a language it cannot parse, because
//! it is a drop-in for upstream's function and refusing a document over our own
//! build configuration would be wrong. But [`is_checkable`] reports the truth,
//! and callers that summarise results are expected to use it to distinguish
//! "checked, clean" from "not checked". [`checkable_languages`] gives the whole
//! set for a build.
//!
//! # The API is upstream's, deliberately
//!
//! [`Language`], [`FencedCodeBlock`], [`SyntaxError`], [`check_syntax`] and
//! [`extract_fenced_code_blocks`] mirror `okf_validator::syntax` item for item,
//! including field names and the tag-to-language mapping, so it is a drop-in and
//! can be deleted here if upstream adopts it.
//!
//! # The accuracy trade, stated honestly
//!
//! tree-sitter is an **error-tolerant** parser. It does not reject input; it
//! builds a tree and marks the parts it could not fit as `ERROR` or `MISSING`
//! nodes. That is a different instrument from a compiler front-end, and it fails
//! in a specific direction: more likely to **accept** something a strict parser
//! rejects than to reject something valid.
//!
//! For code samples in somebody else's documentation that is usually the right
//! direction — a false rejection makes the check unusable against real bundles,
//! a false acceptance merely means it did not fire — and it is what §11 asks
//! for.
//!
//! **Usually, but not always, and the exception decided the feature set.** SQL
//! is handled by `sqlparser` and *not* by a grammar, because
//! `tree-sitter-sequel` rejects **78 of 78** SQL blocks in the four bundles
//! published in the OKF specification repository: it cannot parse `BigQuery`'s
//! backtick-quoted identifiers, which is how essentially every query in that
//! corpus names its table. An error-tolerant parser is not automatically the
//! safer choice; it is only safer where its tolerance covers the dialect in
//! front of it.
//!
//! That was found by running against somebody else's bundles rather than our
//! own fixtures. The synthetic corpus in `tests/accuracy.rs` reported zero false
//! positives while the real one was at 100%, which is worth remembering before
//! trusting any accuracy number here: the cases in that file are the ones we
//! thought to write down.
//!
//! Strictness is not automatically better either, which is why `syn` is not
//! wired up even though it is clean and already in this workspace's lockfile:
//! `syn::parse_file` rejects `let x = 1;`, because a bare statement is not a
//! valid Rust *file*. Documentation is full of fragments, so there the strict
//! parser is the one that gets it wrong. `sqlparser` has the same weakness on
//! the same kind of input — six of the corpus's SQL blocks are bare `ON`
//! clauses or scalar expressions — and it is still the right choice at 6
//! failures against 78. Both measurements are in `tests/accuracy.rs`.

#![forbid(unsafe_code)]

use std::fmt;
use std::str::FromStr;

/// A language a fenced code block can be tagged with.
///
/// The tag mapping is upstream's, including its aliases, so a bundle classifies
/// identically here. Whether a variant can actually be *checked* depends on the
/// features this build enabled — see [`is_checkable`].
///
/// `#[non_exhaustive]`, and this is the one place the crate deliberately differs
/// from `okf_validator::syntax`. Upstream's enum is exhaustive; this one exists
/// to gain variants, because adding a language is adding a backend and that is
/// the crate's whole purpose. A consumer has no reason to match it exhaustively
/// — [`check_syntax`] and [`is_checkable`] are the API — so closing it would
/// promise a stability the design contradicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Language {
    /// Python.
    Python,
    /// JavaScript.
    JavaScript,
    /// TypeScript, including the `tsx`/`jsx` dialects.
    TypeScript,
    /// Rust.
    Rust,
    /// SQL.
    Sql,
    /// JSON.
    Json,
    /// YAML.
    Yaml,
    /// Bash and other POSIX-ish shells.
    Bash,
    /// A tag this crate has no parser for, in any configuration.
    Unknown,
}

/// Every language this crate knows about, checkable or not.
const ALL: [Language; 8] = [
    Language::Python,
    Language::JavaScript,
    Language::TypeScript,
    Language::Rust,
    Language::Sql,
    Language::Json,
    Language::Yaml,
    Language::Bash,
];

impl Language {
    /// Classify an info-string tag (`py`, `python3`, `tsx`, …).
    ///
    /// Case-insensitive. An unrecognised tag is [`Language::Unknown`], never an
    /// error: a bundle may legitimately fence `mermaid`, `text` or nothing at
    /// all, and refusing to classify is not the same as refusing the document.
    #[must_use]
    pub fn from_tag(tag: &str) -> Self {
        match tag.trim().to_ascii_lowercase().as_str() {
            "py" | "python" | "python3" => Self::Python,
            "js" | "javascript" | "node" | "nodejs" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "typescript" | "deno" | "bun" | "mts" | "cts" | "tsx" | "jsx" => {
                Self::TypeScript
            }
            "rs" | "rust" => Self::Rust,
            "sql" => Self::Sql,
            "json" => Self::Json,
            "yaml" | "yml" => Self::Yaml,
            "sh" | "bash" | "zsh" | "shell" => Self::Bash,
            _ => Self::Unknown,
        }
    }

    /// The canonical lower-case name.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Sql => "sql",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Bash => "bash",
            Self::Unknown => "unknown",
        }
    }

    /// Alias for [`Language::as_str`], matching upstream's spelling.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.as_str()
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for Language {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for Language {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_tag(s))
    }
}

/// Whether **this build** can actually parse `language`.
///
/// This is the honest answer, and it moves with the enabled features. A caller
/// summarising a bundle should use it to separate "checked and clean" from "not
/// checked", because reporting the second as the first is how a check becomes
/// vacuous.
#[must_use]
pub const fn is_checkable(language: Language) -> bool {
    match language {
        // No parser needed, so always available.
        Language::Json | Language::Yaml | Language::Bash => true,
        // `sqlparser` only: the grammar is not a fallback here, it is wrong.
        Language::Sql => cfg!(feature = "sql"),
        Language::Python | Language::JavaScript | Language::TypeScript | Language::Rust => {
            cfg!(feature = "grammars")
        }
        Language::Unknown => false,
    }
}

/// Every language this build can check, in a stable order.
///
/// Useful for a `--help` line or a report header: it lets a tool say what it
/// was actually able to look at rather than implying it looked at everything.
#[must_use]
pub fn checkable_languages() -> Vec<Language> {
    ALL.into_iter().filter(|l| is_checkable(*l)).collect()
}

/// One fenced code block lifted out of a markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FencedCodeBlock {
    /// The first word of the info string, if there was one.
    pub language: Option<String>,
    /// The block's contents, without the fences.
    pub code: String,
    /// 1-indexed line of the **opening** fence.
    pub start_line: usize,
}

/// Lift every fenced code block out of a markdown body.
///
/// Handles both backtick and tilde fences, fences indented up to three spaces,
/// and info strings with attributes (`python,linenums=1` classifies as
/// `python`).
///
/// An **unterminated** fence yields no block, matching upstream: a block whose
/// end is unknown has unknown contents, and guessing at them would produce
/// diagnostics about text the author never fenced.
#[must_use]
pub fn extract_fenced_code_blocks(markdown: &str) -> Vec<FencedCodeBlock> {
    let mut blocks = Vec::new();
    let mut in_fence = false;
    let mut fence_char = '`';
    let mut fence_len = 0usize;
    let mut lang: Option<String> = None;
    let mut block_lines: Vec<&str> = Vec::new();
    let mut start_line = 0usize;

    for (line_idx, line) in markdown.lines().enumerate() {
        let line_no = line_idx + 1;
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if in_fence {
            let is_close = indent <= 3 && {
                let count = trimmed.chars().take_while(|&c| c == fence_char).count();
                count >= fence_len && trimmed[count..].trim().is_empty()
            };
            if is_close {
                in_fence = false;
                blocks.push(FencedCodeBlock {
                    language: lang.take(),
                    code: block_lines.join("\n"),
                    start_line,
                });
            } else {
                block_lines.push(line);
            }
        } else if indent <= 3 && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            let ch = trimmed.chars().next().unwrap_or('`');
            let count = trimmed.chars().take_while(|&c| c == ch).count();
            if count >= 3 {
                in_fence = true;
                fence_char = ch;
                fence_len = count;
                start_line = line_no;
                let tag = trimmed[count..].trim();
                let first = tag.split([',', ' ', '\t']).next().unwrap_or("");
                lang = (!first.is_empty()).then(|| first.to_owned());
                block_lines.clear();
            }
        }
    }

    blocks
}

/// A syntax diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// Canonical name of the language that was parsed.
    pub language: String,
    /// What went wrong, in human terms.
    pub message: String,
    /// 1-indexed line within the snippet, when the parser located one.
    pub line: Option<usize>,
    /// 1-indexed column within the snippet, when the parser located one.
    pub column: Option<usize>,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(l), Some(c)) => write!(
                f,
                "{} syntax error at {l}:{c}: {}",
                self.language, self.message
            ),
            (Some(l), None) => write!(
                f,
                "{} syntax error at line {l}: {}",
                self.language, self.message
            ),
            _ => write!(f, "{} syntax error: {}", self.language, self.message),
        }
    }
}

impl std::error::Error for SyntaxError {}

/// Check one code block for syntax errors.
///
/// `language_tag` is the fence's info string.
///
/// Reports `Ok` — never an error — when the tag is unrecognised, when the block
/// is empty, or when **this build has no backend for the language**. Refusing a
/// document over our own build configuration would be wrong, and upstream's
/// function has the same signature. Use [`is_checkable`] to tell the third case
/// apart from a genuine pass.
///
/// # Errors
///
/// [`SyntaxError`] when a backend ran and found the source malformed.
pub fn check_syntax(language_tag: &str, source: &str) -> Result<(), SyntaxError> {
    let language = Language::from_tag(language_tag);
    if source.trim().is_empty() {
        return Ok(());
    }
    match language {
        Language::Json => check_json(source),
        Language::Yaml => check_yaml(source),
        Language::Sql => check_sql(source),
        Language::Bash => check_bash(source),
        Language::Python | Language::JavaScript | Language::TypeScript | Language::Rust => {
            check_with_grammar(language, language_tag, source)
        }
        Language::Unknown => Ok(()),
    }
}

/// SQL, via `sqlparser`, or not at all.
///
/// There is deliberately **no grammar fallback**. `tree-sitter-sequel` rejects
/// every SQL block in the published OKF corpus over `BigQuery`'s backtick-quoted
/// identifiers, so falling back to it would report 78 false errors where
/// reporting "not checked" is honest. A backend that is wrong is worse than a
/// backend that is absent, because [`is_checkable`] can tell a caller about the
/// absence.
#[cfg(feature = "sql")]
fn check_sql(source: &str) -> Result<(), SyntaxError> {
    let dialect = sqlparser::dialect::GenericDialect {};
    sqlparser::parser::Parser::parse_sql(&dialect, source).map_or_else(
        |err| {
            Err(SyntaxError {
                language: "sql".to_owned(),
                message: err.to_string(),
                line: None,
                column: None,
            })
        },
        |_| Ok(()),
    )
}

#[cfg(not(feature = "sql"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by its callers"
)]
fn check_sql(_source: &str) -> Result<(), SyntaxError> {
    Ok(())
}

/// Bash: the grammar when compiled in, otherwise the quoting matcher below.
#[cfg(feature = "grammars")]
fn check_bash(source: &str) -> Result<(), SyntaxError> {
    grammar::check(&tree_sitter_bash::LANGUAGE.into(), Language::Bash, source)
}

#[cfg(not(feature = "grammars"))]
fn check_bash(source: &str) -> Result<(), SyntaxError> {
    check_shell_quoting(source)
}

/// The four grammar-only languages.
#[cfg(feature = "grammars")]
fn check_with_grammar(
    language: Language,
    language_tag: &str,
    source: &str,
) -> Result<(), SyntaxError> {
    let grammar = match language {
        Language::Python => tree_sitter_python::LANGUAGE,
        Language::JavaScript => tree_sitter_javascript::LANGUAGE,
        Language::Rust => tree_sitter_rust::LANGUAGE,
        // The public enum keeps upstream's shape, where `tsx` and `jsx` both
        // classify as TypeScript. The *grammar* still distinguishes them: JSX is
        // a parse error under the plain TypeScript grammar, so collapsing them
        // here would reject valid `tsx` blocks.
        Language::TypeScript => {
            let tag = language_tag.trim().to_ascii_lowercase();
            if matches!(tag.as_str(), "tsx" | "jsx") {
                tree_sitter_typescript::LANGUAGE_TSX
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT
            }
        }
        _ => return Ok(()),
    };
    grammar::check(&grammar.into(), language, source)
}

#[cfg(not(feature = "grammars"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by its callers"
)]
fn check_with_grammar(
    _language: Language,
    _language_tag: &str,
    _source: &str,
) -> Result<(), SyntaxError> {
    Ok(())
}

#[cfg(feature = "grammars")]
mod grammar {
    use tree_sitter::{Node, Parser};

    use super::{Language, SyntaxError};

    /// Parse with a tree-sitter grammar and report the first bad node.
    pub(super) fn check(
        grammar: &tree_sitter::Language,
        language: Language,
        source: &str,
    ) -> Result<(), SyntaxError> {
        let mut parser = Parser::new();
        if parser.set_language(grammar).is_err() {
            // A grammar/runtime ABI mismatch is a build problem, not a defect in
            // the bundle. Reporting it as a syntax error would blame the author
            // for our own dependency, so this reports clean and stays quiet.
            return Ok(());
        }
        let Some(tree) = parser.parse(source, None) else {
            return Ok(());
        };
        let root = tree.root_node();
        if !root.has_error() {
            return Ok(());
        }
        let (line, column, message) = first_bad_node(root).map_or_else(
            || (None, None, "could not parse".to_owned()),
            |node| {
                let pos = node.start_position();
                let what = if node.is_missing() {
                    format!("missing {}", node.kind())
                } else {
                    "unexpected input".to_owned()
                };
                (Some(pos.row + 1), Some(pos.column + 1), what)
            },
        );
        Err(SyntaxError {
            language: language.as_str().to_owned(),
            message,
            line,
            column,
        })
    }

    /// The earliest `ERROR` or `MISSING` node, by byte offset.
    ///
    /// A reader wants the start of the trouble, not whichever node the traversal
    /// happened to reach first.
    fn first_bad_node(root: Node<'_>) -> Option<Node<'_>> {
        let mut stack = vec![root];
        let mut best: Option<Node<'_>> = None;
        while let Some(node) = stack.pop() {
            if node.is_error() || node.is_missing() {
                if best.is_none_or(|b| node.start_byte() < b.start_byte()) {
                    best = Some(node);
                }
                // Children of an error are part of the same trouble and start no
                // earlier than their parent, so there is nothing better inside.
                continue;
            }
            if node.has_error() {
                for i in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(u32::try_from(i).unwrap_or(u32::MAX)) {
                        stack.push(child);
                    }
                }
            }
        }
        best
    }
}

/// JSON, via the serialiser this workspace already uses.
fn check_json(source: &str) -> Result<(), SyntaxError> {
    serde_json::from_str::<serde_json::Value>(source).map_or_else(
        |err| {
            Err(SyntaxError {
                language: "json".to_owned(),
                message: err.to_string(),
                line: Some(err.line()),
                column: Some(err.column()),
            })
        },
        |_| Ok(()),
    )
}

/// YAML, via `okf-core`'s parser — the same one that read the frontmatter.
///
/// Deliberately not a second YAML implementation: a bundle whose frontmatter
/// parsed but whose fenced YAML did not, because two parsers disagreed, would be
/// a defect of ours rather than of the document.
fn check_yaml(source: &str) -> Result<(), SyntaxError> {
    okf_core::yaml::Value::parse(source).map_or_else(
        |err| {
            Err(SyntaxError {
                language: "yaml".to_owned(),
                message: err.to_string(),
                line: None,
                column: None,
            })
        },
        |_| Ok(()),
    )
}

/// Shell checking without a grammar: quoting and bracket balance only.
///
/// Deliberately weak. This runs only in a `--no-default-features` build, where
/// the alternative is no check at all, and it is written to be **conservative**
/// — it reports trouble only for an unterminated quote or an unclosed bracket,
/// both of which are unambiguous. It will miss an unterminated `do`/`done`,
/// which the grammar catches.
///
/// Keyword pairing is not attempted on purpose: `;;` in a `case`, heredocs and
/// `$((` arithmetic make it easy to write a matcher that rejects valid scripts,
/// and a false rejection is the one failure this crate must not have.
#[cfg_attr(feature = "grammars", expect(dead_code))]
fn check_shell_quoting(source: &str) -> Result<(), SyntaxError> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    // `usize`, so the saturating close below genuinely floors at zero rather
    // than at `i32::MIN` — which would have made `) (` cancel out and hide a
    // real unclosed paren.
    let mut depth: usize = 0;
    let mut line = 1usize;
    let mut quote_line = 1usize;

    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        // Escape state is consumed **before** the newline branch, deliberately.
        // A backslash before a newline is a POSIX line continuation: it escapes
        // the newline itself, so the next line begins unescaped. Checking the
        // newline first left `escaped` set across the break, which then swallowed
        // the first character of the following line — and swallowing an opening
        // quote or paren is exactly how this matcher would produce the false
        // rejection it exists to avoid.
        if escaped {
            escaped = false;
            if c == '\n' {
                line += 1;
            }
            continue;
        }
        if c == '\n' {
            line += 1;
            // A newline ends a comment but not a quote: shell strings span lines.
            continue;
        }
        // Backslash escapes everywhere except inside single quotes.
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        // Inside a quote nothing is special but the matching close. Handled
        // before the unquoted cases so the two states never share a match arm.
        if let Some(open) = quote {
            if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '#' => {
                // Comment to end of line.
                for c in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '\'' | '"' => {
                quote = Some(c);
                quote_line = line;
            }
            // **Parentheses only, and only in the unclosed direction.**
            //
            // `[`, `]`, `{` and `}` are ordinary arguments in shell far more
            // often than they are structure — `echo ]` and `echo }` are both
            // valid — so counting them produced exactly the false rejection this
            // matcher exists to avoid.
            //
            // A *closing* paren is no safer: `case x in a) ;; esac` is valid
            // shell whose `)` has no opener, and `;;` patterns make that shape
            // common. So a close saturates at zero rather than going negative,
            // and only an **unclosed** `(` is reported. That keeps the case
            // worth catching — an unterminated `$(` — and drops the direction
            // that produces false positives.
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    if let Some(q) = quote {
        return Err(SyntaxError {
            language: "bash".to_owned(),
            message: format!("unterminated {q} quote"),
            line: Some(quote_line),
            column: None,
        });
    }
    if depth > 0 {
        return Err(SyntaxError {
            language: "bash".to_owned(),
            message: "unclosed `(`".to_owned(),
            line: None,
            column: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest is the contract. This crate is worth having only while it
    /// stays free of the tree that made `okf-validator` unusable, so the
    /// dependency list is asserted rather than trusted.
    ///
    /// If you are here because this failed: adding a dependency is allowed, but
    /// it is a decision. Check it against `deny.toml` first; if it parses a
    /// programming language it belongs behind a feature, not in the core; and if
    /// it brings a language front-end, ask whether a grammar already covers it.
    #[test]
    fn dependencies_are_frozen() {
        let manifest = include_str!("../Cargo.toml");
        let deps: Vec<String> = manifest
            .lines()
            .skip_while(|l| l.trim() != "[dependencies]")
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('['))
            .filter_map(|l| {
                let line = l.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                line.split(['=', ' ']).next().map(str::to_owned)
            })
            .collect();
        let mut sorted = deps.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![
                "okf-core".to_owned(),
                "serde_json".to_owned(),
                "sqlparser".to_owned(),
                "tree-sitter".to_owned(),
                "tree-sitter-bash".to_owned(),
                "tree-sitter-javascript".to_owned(),
                "tree-sitter-python".to_owned(),
                "tree-sitter-rust".to_owned(),
                "tree-sitter-typescript".to_owned(),
            ],
            "the dependency list changed; see this test's documentation"
        );
    }

    /// The core must stay free of code parsers, which is the crate's reason to
    /// exist. Anything that parses a programming language is `optional = true`.
    #[test]
    fn every_language_parser_is_optional() {
        let manifest = include_str!("../Cargo.toml");
        let non_optional: Vec<&str> = manifest
            .lines()
            .skip_while(|l| l.trim() != "[dependencies]")
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('['))
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#') && !t.contains("optional = true")
            })
            .filter_map(|l| l.trim().split([' ', '=']).next())
            .collect();
        assert_eq!(
            non_optional,
            vec!["okf-core", "serde_json"],
            "only the two pure-Rust, non-code-parsing dependencies may be \
             mandatory; everything that parses a language belongs behind a feature"
        );
    }

    /// A build always knows what it can and cannot do, and says so.
    #[test]
    fn the_checkable_set_matches_the_features() {
        let set = checkable_languages();
        // No parser needed, so these hold in every configuration.
        for always in [Language::Json, Language::Yaml, Language::Bash] {
            assert!(set.contains(&always), "{always} must always be checkable");
        }
        assert!(
            !set.contains(&Language::Unknown),
            "Unknown is never checkable"
        );
        assert_eq!(
            set.contains(&Language::Python),
            cfg!(feature = "grammars"),
            "Python is checkable exactly when the grammars are compiled in"
        );
        assert_eq!(
            set.contains(&Language::Sql),
            cfg!(feature = "sql"),
            "SQL is checkable exactly when `sqlparser` is compiled in — the \
             grammar is not a fallback, because it is wrong for this corpus"
        );
    }

    #[test]
    fn an_unknown_tag_is_not_an_error() {
        assert_eq!(Language::from_tag("mermaid"), Language::Unknown);
        assert!(check_syntax("mermaid", "graph TD; A-->B;").is_ok());
        assert!(check_syntax("", "anything at all {{{").is_ok());
    }

    #[test]
    fn tags_are_case_insensitive_and_aliased() {
        assert_eq!(Language::from_tag("PY"), Language::Python);
        assert_eq!(Language::from_tag("Python3"), Language::Python);
        assert_eq!(Language::from_tag(" TSX "), Language::TypeScript);
        assert_eq!(Language::from_tag("yml"), Language::Yaml);
    }

    #[test]
    fn an_empty_block_is_clean() {
        assert!(check_syntax("python", "").is_ok());
        assert!(check_syntax("python", "   \n\n").is_ok());
    }

    #[test]
    fn json_and_yaml_need_no_feature() {
        assert!(check_syntax("json", r#"{"a": [1, 2]}"#).is_ok());
        assert!(check_syntax("json", r#"{"a": }"#).is_err());
        assert!(check_syntax("yaml", "a: 1\nb:\n  - c\n").is_ok());
    }

    #[test]
    fn fences_are_extracted_with_their_language_and_line() {
        let md = "intro\n\n```python\nx = 1\n```\n\ntext\n\n~~~sql\nSELECT 1;\n~~~\n";
        let blocks = extract_fenced_code_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].language.as_deref(), Some("python"));
        assert_eq!(blocks[0].code, "x = 1");
        assert_eq!(blocks[0].start_line, 3);
        assert_eq!(blocks[1].language.as_deref(), Some("sql"));
    }

    #[test]
    fn an_info_string_with_attributes_still_classifies() {
        let md = "```python,linenums=1\nx = 1\n```\n";
        let blocks = extract_fenced_code_blocks(md);
        assert_eq!(blocks[0].language.as_deref(), Some("python"));
    }

    /// An unterminated fence yields nothing, matching upstream: the block's end
    /// is unknown, so its contents are too.
    #[test]
    fn an_unterminated_fence_yields_no_block() {
        assert!(extract_fenced_code_blocks("```python\nx = 1\n").is_empty());
    }

    /// A fence inside a longer fence is content, not a delimiter.
    #[test]
    fn a_shorter_inner_fence_does_not_close_the_block() {
        let md = "````markdown\n```\ninner\n```\n````\n";
        let blocks = extract_fenced_code_blocks(md);
        assert_eq!(blocks.len(), 1, "one block, not three: {blocks:?}");
        assert!(blocks[0].code.contains("inner"));
    }

    /// The featureless shell matcher: conservative, but not useless.
    ///
    /// Tested directly rather than through [`check_syntax`], so the behaviour is
    /// pinned in every configuration instead of only in the one build where it
    /// is reachable.
    #[test]
    fn the_featureless_shell_matcher_catches_only_the_unambiguous() {
        #[cfg_attr(feature = "grammars", expect(unused_imports))]
        use super::check_shell_quoting as check;
        #[cfg(not(feature = "grammars"))]
        {
            assert!(check("echo \"hi\"\n").is_ok());
            assert!(check("echo 'it'\\''s'\n").is_ok());
            assert!(check("a=$(b | c)\n").is_ok());
            assert!(check("# a comment with an ' apostrophe\n").is_ok());
            assert!(check("if [ -f x ]; then echo y; fi\n").is_ok());
            assert!(check("echo \"unterminated\n").is_err());
            assert!(check("a=$(b\n").is_err());

            // A backslash before a newline is a line continuation: it escapes
            // the newline, so the next line starts unescaped. Consuming the
            // escape *after* the newline branch left it set across the break and
            // swallowed the next line's first character — here the opening
            // quote, which then read as unbalanced.
            assert!(
                check("echo one \\\n\"two\"\n").is_ok(),
                "a line continuation must not swallow the next line's first char"
            );

            // `[`, `]`, `{` and `}` are ordinary arguments far more often than
            // they are structure, so they are not counted. Counting them is how
            // this matcher would reject valid input.
            for ok in ["echo ]\n", "echo }\n", "echo [\n", "case x in a) ;; esac\n"] {
                assert!(check(ok).is_ok(), "must not reject valid shell: {ok:?}");
            }

            // A stray close saturates at zero rather than cancelling a later
            // open, so this is still caught.
            assert!(
                check(") a=$(b\n").is_err(),
                "a close must not license an open"
            );
        }
    }

    #[cfg(feature = "grammars")]
    #[test]
    fn an_error_carries_a_position() {
        let err = check_syntax("python", "def f(x)\n    return x\n").expect_err("missing colon");
        assert_eq!(err.language, "python");
        assert!(
            err.line.is_some(),
            "the parser located the trouble: {err:?}"
        );
        assert!(err.to_string().contains("python syntax error"), "{err}");
    }

    /// The reported position is the *first* trouble, not an arbitrary node.
    #[cfg(feature = "grammars")]
    #[test]
    fn the_position_points_at_the_first_error() {
        let err = check_syntax("python", "x = 1\ny = 2\nz = (\n").expect_err("unclosed paren");
        assert_eq!(
            err.line,
            Some(3),
            "the first two lines are fine; the trouble starts on line 3: {err:?}"
        );
    }

    /// `tsx` must parse with the TSX grammar. The plain TypeScript grammar
    /// rejects JSX, so collapsing the two would reject valid blocks — this is
    /// the one place the public enum's shape and the grammar's differ.
    #[cfg(feature = "grammars")]
    #[test]
    fn jsx_is_parsed_with_the_tsx_grammar() {
        let jsx = "const a = <div className=\"x\">hi</div>;\n";
        assert!(check_syntax("tsx", jsx).is_ok(), "tsx must accept JSX");
        assert!(check_syntax("jsx", jsx).is_ok(), "jsx must accept JSX");
    }

    /// The shape of real OKF SQL, which is `BigQuery`'s.
    ///
    /// This is the case that a synthetic corpus missed entirely and that decided
    /// the backend: `tree-sitter-sequel` rejects it, `sqlparser` accepts it, and
    /// essentially every query in the published bundles is written this way.
    #[cfg(feature = "sql")]
    #[test]
    fn backtick_quoted_bigquery_identifiers_are_accepted() {
        let real = "SELECT\n  block_timestamp,\n  value / 100000000 AS value_btc\n\
                    FROM `bigquery-public-data.crypto_bitcoin.inputs`\n\
                    WHERE block_timestamp >= '2024-04-17 00:00:00 UTC'\n\
                    ORDER BY value DESC\nLIMIT 10;\n";
        assert!(
            check_syntax("sql", real).is_ok(),
            "this is the dominant shape in the real corpus and must not be rejected"
        );
    }

    /// Nonsense is still caught, so the SQL arm is not merely permissive.
    #[cfg(feature = "sql")]
    #[test]
    fn the_sql_arm_still_rejects_nonsense() {
        assert!(check_syntax("sql", "SELECT FROM;\n").is_err());
        assert!(check_syntax("sql", "SELCT a FROM t;\n").is_err());
    }
}

//! Scanning source files for `@rto:<id>` annotations, which link code back to
//! the ADR that authored or governs it.
//!
//! An annotation is any `@rto:<id>` token in a file (typically in a `//` or
//! `//!` comment); the scan is comment-agnostic and simply finds the token so
//! it works across languages.

/// A `@rto:<id>` annotation found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    /// Repository-relative path of the file the annotation is in.
    pub path: String,
    /// The referenced ADR id.
    pub adr_id: String,
    /// 1-based line number.
    pub line: usize,
}

/// The graph node key this annotation targets (`adr:<id>`).
impl Annotation {
    /// The ADR node key this annotation references.
    #[must_use]
    pub fn target_key(&self) -> String {
        format!("adr:{}", self.adr_id)
    }
}

const MARKER: &str = "@rto:";

/// Whether a line is a comment, per a small set of prefixes covering the
/// languages Roteiro ingests. Annotations are only recognised on comment lines
/// so that example `@rto:<id>` tokens inside string literals (e.g. test
/// fixtures) are not mistaken for real annotations.
fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    ["//", "#", "*", "/*", "<!--", ";", "--"]
        .iter()
        .any(|p| t.starts_with(p))
}

/// Find every `@rto:<id>` annotation on a comment line in `text`, tagged with
/// `rel_path`.
#[must_use]
pub fn scan_annotations(rel_path: &str, text: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if !is_comment_line(line) {
            continue;
        }
        let mut rest = line;
        while let Some(pos) = rest.find(MARKER) {
            let after = &rest[pos + MARKER.len()..];
            let id: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !id.is_empty() {
                out.push(Annotation {
                    path: rel_path.to_owned(),
                    adr_id: id.clone(),
                    line: i + 1,
                });
            }
            // Advance past this marker (plus the id) to find more on one line.
            rest = &after[id.len()..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::scan_annotations;

    #[test]
    fn finds_annotations_with_line_numbers() {
        let src = "//! @rto:0001\nfn a() {}\n// see @rto:0042 and @rto:0007 here\n";
        let anns = scan_annotations("src/lib.rs", src);
        assert_eq!(anns.len(), 3);
        assert_eq!(anns[0].adr_id, "0001");
        assert_eq!(anns[0].line, 1);
        assert_eq!(anns[0].target_key(), "adr:0001");
        assert_eq!(anns[1].adr_id, "0042");
        assert_eq!(anns[1].line, 3);
        assert_eq!(anns[2].adr_id, "0007");
    }

    #[test]
    fn ignores_bare_marker_without_id() {
        assert!(scan_annotations("x.rs", "// @rto: nothing\n").is_empty());
    }

    #[test]
    fn ignores_annotations_outside_comments() {
        // An `@rto:` inside a string literal on a code line is not an annotation.
        let src = "let s = \"@rto:9999\";\n// @rto:0001\n";
        let anns = scan_annotations("src/x.rs", src);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].adr_id, "0001");
    }
}

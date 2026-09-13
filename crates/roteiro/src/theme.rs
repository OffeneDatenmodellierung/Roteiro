//! Roteiro's **ONE token master** for its in-app web surfaces.
//!
//! `assets/tokens.css` is a single definition of the app palette — light and
//! dark — and every in-app surface is themed from it and from nothing else:
//!
//! - the workspace hub and the project graph view, spliced into the explorer
//!   shell's inline `<style>` ([`crate::explorer_app`]);
//! - the OKF viewer, prepended to its served stylesheet ([`crate::okf_viewer`]);
//! - the `links --matrix --html` export, inlined into the page
//!   ([`crate::overview::render_html`]).
//!
//! It lives here, under the crate directory, precisely so all three can reach it
//! with `include_str!`. That was the constraint that used to force the viewer to
//! keep a hand-copied byte duplicate of `website/public/style.css`: `cargo
//! package` takes only files under the crate directory, so an include reaching
//! out to `website/` would ship a crate that does not compile. Nothing reaches
//! out of the crate now.
//!
//! Two things this module deliberately does **not** do. It does not theme
//! `website/public/style.css` — the public docs site is a separate artefact with
//! its own lifecycle, and making a published site depend on an application
//! binary's assets would be the same drift risk pointing the other way. And it
//! does not serve the tokens over a route: the matrix export is self-contained
//! by requirement, so the master has to be inlinable at build time.

/// The palette, verbatim. Inlined into every in-app surface.
///
/// Consumers must not reformat, re-minify or partially copy it — the guards
/// (`the_viewer_is_themed_by_the_app_token_master`,
/// `the_shell_splices_in_the_token_master`, `the_matrix_export_inlines_the_token_master`)
/// compare against these bytes.
pub(crate) const TOKENS: &str = include_str!("assets/tokens.css");

/// The custom-property names [`TOKENS`] declares, e.g. `--bg`.
///
/// Parsed rather than listed, so adding a token to the master cannot forget to
/// add it here. Used by the drift guards to answer "is this `var(--x)` real?".
#[cfg(test)]
pub(crate) fn declared_names() -> std::collections::BTreeSet<String> {
    TOKENS
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("--"))
        .filter_map(|l| l.split_once(':'))
        .map(|(name, _)| format!("--{}", name.trim()))
        .collect()
}

/// CSS with its comments removed.
///
/// Prose is not markup. An issue number reads as a colour literal (`#512`), a
/// brace in a sentence reads as a block, and this file's own history — which
/// names the palette it replaced — reads as a palette. Every scanner below works
/// on the stripped text, so what a comment says can never change what a guard
/// concludes.
#[cfg(test)]
pub(crate) fn without_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(open) = rest.find("/*") {
        out.push_str(&rest[..open]);
        match rest[open + 2..].find("*/") {
            Some(close) => rest = &rest[open + 2 + close + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Properties that **cannot** carry a `<color>`, so their values are not scanned.
///
/// This list is the allowlist, and the default is therefore *scanned*: a
/// property nobody thought of — `caret-color`, `accent-color`, `scrollbar-color`,
/// whatever CSS adds next — is colour-checked without anyone remembering to say
/// so. Adding a genuinely new layout or typography property costs one line here,
/// and the failure says exactly that. That friction is the point: an incomplete
/// allowlist refuses visibly, where an incomplete denylist passes silently.
#[cfg(test)]
const NON_COLOUR_PROPERTIES: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "appearance",
    "aspect-ratio",
    "border-collapse",
    "border-radius",
    "border-style",
    "bottom",
    "box-sizing",
    "clip-path",
    "column-gap",
    "content",
    "cursor",
    "direction",
    "display",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant-numeric",
    "font-weight",
    "gap",
    "grid-area",
    "grid-column",
    "grid-row",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "inset",
    "isolation",
    "justify-content",
    "justify-self",
    "left",
    "letter-spacing",
    "line-height",
    "list-style",
    "margin",
    "margin-bottom",
    "margin-inline",
    "margin-left",
    "margin-right",
    "margin-top",
    "mask",
    "max-block-size",
    "max-height",
    "max-width",
    "min-block-size",
    "min-height",
    "min-width",
    "object-fit",
    "opacity",
    "order",
    "outline-offset",
    "overflow",
    "overflow-wrap",
    "overflow-x",
    "overflow-y",
    "padding",
    "padding-bottom",
    "padding-inline",
    "padding-left",
    "padding-right",
    "padding-top",
    "pointer-events",
    "position",
    "resize",
    "right",
    "rotate",
    "row-gap",
    "scale",
    "scroll-behavior",
    "tab-size",
    "table-layout",
    "text-align",
    "text-indent",
    "text-overflow",
    "text-transform",
    "text-wrap",
    "top",
    "touch-action",
    "transform",
    "transition",
    "translate",
    "user-select",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "will-change",
    "word-break",
    "word-spacing",
    "writing-mode",
    "z-index",
];

/// Bare words a colour-bearing value may contain.
///
/// None of them names a colour. `transparent` and `currentcolor` are colour
/// *keywords*, and are here on purpose: neither carries a palette value, so
/// neither can fork the identity — which is the thing being protected.
#[cfg(test)]
const VALUE_KEYWORDS: &[&str] = &[
    "at",
    "auto",
    "blink",
    "bottom",
    "center",
    "circle",
    "closest-corner",
    "closest-side",
    "corner",
    "currentcolor",
    "dashed",
    "dotted",
    "double",
    "ellipse",
    "farthest-corner",
    "farthest-side",
    "from-font",
    "groove",
    "hidden",
    "inherit",
    "initial",
    "inset",
    "left",
    "line-through",
    "medium",
    "none",
    "outset",
    "overline",
    "revert",
    "revert-layer",
    "ridge",
    "right",
    "solid",
    "thick",
    "thin",
    "to",
    "top",
    "transparent",
    "underline",
    "unset",
    "wavy",
];

/// Functions a colour-bearing value may wrap its parts in.
///
/// Deliberately **not** `rgb`/`hsl`/`oklch`/`color-mix`/`light-dark` — a colour
/// belongs in `tokens.css`, and any function not named here is reported whether
/// or not it existed when this was written.
#[cfg(test)]
const VALUE_FUNCTIONS: &[&str] = &[
    "calc",
    "clamp",
    "conic-gradient",
    "env",
    "linear-gradient",
    "max",
    "min",
    "radial-gradient",
    "repeating-conic-gradient",
    "repeating-linear-gradient",
    "repeating-radial-gradient",
];

/// Every `property: value` declaration in a stylesheet, comments stripped.
///
/// A flat walk, not a parser: `{` starts a body (so the selector prelude is
/// dropped), `;` and `}` each end a declaration. That is enough for the four
/// hand-written stylesheets this guards, and it is the same shape `css_scopes`
/// uses for the palette.
#[cfg(test)]
pub(crate) fn declarations(css: &str) -> Vec<(String, String)> {
    let css = without_comments(css);
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in css.chars() {
        match ch {
            '{' => buf.clear(),
            ';' | '}' => {
                if let Some((prop, value)) = buf.split_once(':') {
                    let prop = prop.trim();
                    // A selector or at-rule prelude can reach here on malformed
                    // input; a real property is one identifier.
                    if !prop.is_empty() && !prop.contains(char::is_whitespace) {
                        out.push((prop.to_ascii_lowercase(), value.trim().to_owned()));
                    }
                }
                buf.clear();
            }
            _ => buf.push(ch),
        }
    }
    out
}

/// Every custom property a document *references*, fallbacks included.
///
/// The name runs to the first `,` or `)` — so `var(--x, fallback)` yields `--x`
/// rather than being skipped, which is what the previous version did and is how
/// an undeclared `var(--mono, …)` sat in the shell unnoticed. A `var()` nested
/// inside a fallback is found by the same linear scan, as its own `var(`.
#[cfg(test)]
pub(crate) fn referenced_tokens(text: &str) -> Vec<String> {
    let text = without_comments(text);
    let mut out: Vec<String> = text
        .match_indices("var(")
        .map(|(i, _)| &text[i + "var(".len()..])
        .map(|rest| {
            let end = rest.find([',', ')']).unwrap_or(rest.len());
            rest[..end].trim().to_owned()
        })
        .filter(|name| name.starts_with("--"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every `var(--x)` a document names that [`TOKENS`] does not declare.
///
/// These are the silent failure #512 records: an undefined custom property is
/// invalid at computed-value time rather than an error, so it falls back to
/// inherited or initial and renders plausibly wrong.
#[cfg(test)]
pub(crate) fn dangling_tokens(text: &str) -> Vec<String> {
    let declared = declared_names();
    referenced_tokens(text)
        .into_iter()
        .filter(|name| !declared.contains(name))
        .collect()
}

/// Split a value into the tokens the allowlist judges, with `var(--name)`
/// references removed.
///
/// Removing the reference — not the whole `var()` — is deliberate: a fallback
/// stays in the stream, so `color: var(--x, #fff)` still reports `#fff`.
#[cfg(test)]
fn value_tokens(value: &str) -> Vec<String> {
    // Drop `var(` and the custom-property name, keeping any fallback.
    let mut stripped = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find("var(") {
        stripped.push_str(&rest[..i]);
        let after = &rest[i + "var(".len()..];
        let end = after.find([',', ')']).unwrap_or(after.len());
        if after[..end].trim().starts_with("--") {
            rest = &after[end..];
        } else {
            // Not a custom-property reference; leave it to be judged.
            stripped.push_str("var(");
            rest = after;
        }
    }
    stripped.push_str(rest);

    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in stripped.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.' | '%' | '#' => cur.push(ch),
            '(' => {
                // A function name is judged with its parenthesis attached, so a
                // bare keyword and a call can never be confused.
                if !cur.is_empty() {
                    out.push(format!("{}(", std::mem::take(&mut cur)));
                }
            }
            _ => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A token is a number, a dimension or a percentage (`0`, `.3rem`, `-1px`, `50%`).
#[cfg(test)]
fn is_numeric(token: &str) -> bool {
    let body = token.strip_prefix(['-', '+']).unwrap_or(token);
    body.starts_with(|c: char| c.is_ascii_digit() || c == '.')
}

/// Colours a stylesheet states for itself instead of naming a token.
///
/// **An allowlist in both directions**, which is the whole point — the previous
/// version recognised `#rrggbb` and read every other syntax as absence, so two
/// `rgba()` shadows sat in the shell while the guard stayed green.
///
/// 1. A declaration is scanned unless its property is in
///    [`NON_COLOUR_PROPERTIES`]. Default: scanned.
/// 2. In a scanned value, every token must be a `var(--…)` reference, a number
///    or dimension, a word in [`VALUE_KEYWORDS`], or a function in
///    [`VALUE_FUNCTIONS`]. Default: reported.
///
/// So `rgba(…)`, `#abc`, `red`, `oklch(…)`, `color-mix(…)` and whatever CSS
/// adds next are all reported, without anyone having had to think of them.
#[cfg(test)]
pub(crate) fn colour_literals(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (prop, value) in declarations(css) {
        if NON_COLOUR_PROPERTIES.contains(&prop.as_str()) || prop.starts_with("--") {
            continue;
        }
        for token in value_tokens(&value) {
            let lower = token.to_ascii_lowercase();
            let ok = if let Some(name) = lower.strip_suffix('(') {
                VALUE_FUNCTIONS.contains(&name)
            } else {
                is_numeric(&lower) || VALUE_KEYWORDS.contains(&lower.as_str())
            };
            if !ok {
                out.push(format!("{prop}: …{token}…"));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

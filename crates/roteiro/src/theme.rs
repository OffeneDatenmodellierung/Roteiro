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

/// The colour literals a stylesheet hard-codes, comments excluded.
///
/// Any of these is a surface quietly forking the identity: it renders perfectly,
/// and it stops answering to [`TOKENS`].
#[cfg(test)]
pub(crate) fn hex_literals(css: &str) -> Vec<String> {
    let css = without_comments(css);
    css.match_indices('#')
        .map(|(i, _)| &css[i + 1..])
        .map(|rest| {
            let end = rest
                .find(|c: char| !c.is_ascii_hexdigit())
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .filter(|tok| matches!(tok.len(), 3 | 4 | 6 | 8))
        .map(|tok| format!("#{tok}"))
        .collect()
}

/// Every `var(--x)` a document names that [`TOKENS`] does not declare.
///
/// A `var()` carrying a fallback is ignored — there, the fallback IS the
/// declared value. The rest are the silent failure #512 records: an undefined
/// custom property is invalid at computed-value time rather than an error, so it
/// falls back to inherited or initial and renders plausibly wrong.
#[cfg(test)]
pub(crate) fn dangling_tokens(text: &str) -> Vec<String> {
    let declared = declared_names();
    // Comments stripped for the reason [`without_comments`] gives: this file's
    // own prose talks ABOUT `var()` and must not be read as using one.
    let text = without_comments(text);
    let mut out: Vec<String> = text
        .match_indices("var(--")
        .map(|(i, _)| &text[i + "var(".len()..])
        .map(|rest| &rest[..rest.find(')').unwrap_or(rest.len())])
        .filter(|inner| !inner.contains(','))
        .map(|name| name.trim().to_owned())
        .filter(|name| !declared.contains(name))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

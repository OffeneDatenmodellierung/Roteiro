//! The licence line for everything Roteiro offers to download (#904).
//!
//! Disclosure alone prints a licence; this module decides whether it is
//! acceptable. Both registries — models (`rto-graph`'s `models` feature) and
//! security assets (`rto-exec`) — are checked against it by their own tests,
//! so it lives outside the `models` feature gate.

/// SPDX licence ids any provisionable model or security asset may carry.
///
/// An **allowlist**, not a denylist: a list of the unacceptable cannot be
/// finished, and a use-restricted licence (`OpenRAIL-M`, `CC-BY-NC-4.0`, a
/// community licence) must be refused by default rather than printed and
/// accepted. The line is *permissive*: the software ids are the ones
/// `deny.toml` allows for crates, plus the two permissive content licences the
/// shipped data needs (`CC0-1.0`, `CC-BY-4.0`). Anything outside it needs an
/// argued entry in [`LICENCE_EXCEPTIONS`].
pub const PERMISSIVE_LICENCES: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "CC0-1.0",
    "CC-BY-4.0",
];

/// Argued exceptions to [`PERMISSIVE_LICENCES`]: `(model or asset id, SPDX id,
/// why)`. Scoped to one id, so a second model under the same licence is still
/// refused until someone argues for it too.
pub const LICENCE_EXCEPTIONS: &[(&str, &str, &str)] = &[(
    "ocrs-text",
    "CC-BY-SA-4.0",
    // Share-alike is copyleft for *content*, not a use restriction: it binds
    // redistributed adaptations of the weights, which Roteiro never makes — it
    // downloads the published files on consent and runs them unmodified.
    "share-alike binds adaptations of the weights; Roteiro downloads and runs them unmodified",
)];

/// Check that `licence` — an SPDX expression, optionally followed by a
/// parenthesised human note — is built only from [`PERMISSIVE_LICENCES`] or an
/// exception argued for `id`.
///
/// # Errors
///
/// A message naming `id` and the offending licence term, so a failing gate says
/// which model to look at and why.
pub fn check_licence(id: &str, licence: &str) -> Result<(), String> {
    let expr = licence.split(" (").next().unwrap_or_default().trim();
    if expr.is_empty() {
        return Err(format!("{id}: discloses no SPDX licence (`{licence}`)"));
    }
    for term in expr.split(" OR ").flat_map(|t| t.split(" AND ")) {
        let term = term.trim();
        let excepted = LICENCE_EXCEPTIONS
            .iter()
            .any(|&(eid, lic, _)| eid == id && lic == term);
        if !PERMISSIVE_LICENCES.contains(&term) && !excepted {
            return Err(format!(
                "{id}: licence `{term}` is not on the permissive allowlist \
                 (PERMISSIVE_LICENCES) and has no argued exception (LICENCE_EXCEPTIONS)"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_licence;

    /// The checker refuses a use-restricted licence and names model and
    /// licence, accepts SPDX `OR`/`AND` of allowed ids with a trailing note, and
    /// does not let one model's argued exception cover another.
    #[test]
    fn check_licence_refuses_what_is_not_on_the_allowlist() {
        let err = check_licence("chandra", "OpenRAIL-M").expect_err("refused");
        assert!(
            err.contains("chandra") && err.contains("OpenRAIL-M"),
            "{err}"
        );
        assert!(check_licence("x", "MIT OR Apache-2.0 (note)").is_ok());
        assert!(check_licence("x", "CC0-1.0 AND CC-BY-4.0").is_ok());
        // Strict per term, even under `OR`: a disjunct nobody may use is a
        // smell worth a human look, not a free pass.
        assert!(check_licence("x", "MIT OR CC-BY-NC-4.0").is_err());
        assert!(check_licence("x", "").is_err());
        assert!(check_licence("ocrs-text", "CC-BY-SA-4.0").is_ok());
        assert!(check_licence("other-ocr", "CC-BY-SA-4.0").is_err());
    }
}

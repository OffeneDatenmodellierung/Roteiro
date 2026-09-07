//! How a committed project file may narrow a setting but never widen it
//! (ADR-0007 v1.4).
//!
//! The ADR's rule is stated for a switch — *"a project may deny but never
//! grant"* — because the first key it governed was one. But `[remote] enabled`
//! and `[serve] max_client_tool_bytes` are the same rule at different widths: a
//! project file may move the value toward **more** restriction and never toward
//! less. Under `Ord` that is one comparison, since `false < true` and a smaller
//! bound is a tighter one.
//!
//! ADR-0007 §111 asks for exactly this: *"A capability key's layering must
//! therefore be carried by its **type**, so that declaring a key a capability
//! and getting its precedence right are the same act"* — and warns that a second
//! bespoke implementation is how the rule decays into a convention. [`Grant`] is
//! that type, and it is the only place the rule is written.

/// Two config layers for one capability key, with the ADR-0007 v1.4 inversion.
///
/// Construct with [`Grant::from_layers`] and read with [`Grant::as_effective`].
/// The type parameter's `Ord` **is** the rule: `T`'s smaller value is its more
/// restrictive one, so `bool` works because `false < true` and a byte bound
/// works because a lower ceiling grants less.
///
/// `default` is the built-in — what the key means with neither layer set. It is
/// needed because "the project may not widen" is only meaningful against a
/// baseline: a project naming the built-in default widens nothing, and one
/// naming anything above it is a grant, whether or not a user layer exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grant<T> {
    project: Option<T>,
    user: Option<T>,
    default: T,
}

impl<T: Ord + Copy> Grant<T> {
    /// Read the two layers against the built-in `default`.
    ///
    /// `project` is the committed file's value (`roteiro.toml`), `user` is the
    /// machine owner's (`~/.roteiro/config.toml`). Both are `None` when absent.
    #[must_use]
    pub fn from_layers(project: Option<T>, user: Option<T>, default: T) -> Self {
        Self {
            project,
            user,
            default,
        }
    }

    /// What the user layer asked for, or the built-in when it is silent. The
    /// baseline the project is measured against.
    fn baseline(self) -> T {
        self.user.unwrap_or(self.default)
    }

    /// The layers' effective contribution, or `None` when neither layer spoke.
    ///
    /// `None` is preserved rather than resolved to `default` because "unset" and
    /// "set to the default value" are the same behaviour but not the same
    /// report, and `roteiro config` echoes this: telling someone a key is set
    /// when nobody set it describes a decision that was never taken.
    #[must_use]
    pub fn as_effective(self) -> Option<T> {
        match self.project {
            // Applies when it does not widen — `<=` and not `<` so an explicit
            // project denial is still reported as one even where it merely
            // restates the baseline.
            Some(p) if p <= self.baseline() => Some(p),
            // Either the project tried to widen (discarded) or said nothing.
            _ => self.user,
        }
    }

    /// The value to act on, with the built-in applied.
    #[must_use]
    pub fn resolved(self) -> T {
        self.as_effective().unwrap_or(self.default)
    }

    /// Whether the project's value failed to apply because it would have
    /// widened the key.
    ///
    /// Worth surfacing rather than silently dropping: a project author who set
    /// this believes it took effect, and the whole point of the inversion is
    /// that it did not.
    ///
    /// **Delta semantics, deliberately.** This asks whether the value applied,
    /// not what the file literally said — so a project naming a bound *above*
    /// the user's own raise is ignored, while one naming a bound between the
    /// built-in and that raise applies. A caller wanting "the file said X" should
    /// test the file, not this; `ConfigGrant` keeps such a predicate of its own
    /// for `[remote] enabled`, where "denied" means the literal `false`.
    #[must_use]
    pub fn project_widening_ignored(self) -> bool {
        matches!(self.project, Some(p) if p > self.baseline())
    }

    /// Whether the project narrowed this key below what the baseline allowed —
    /// the case the inversion exists to permit.
    #[must_use]
    pub fn project_narrowed(self) -> bool {
        matches!(self.project, Some(p) if p < self.baseline())
    }
}

#[cfg(test)]
mod tests {
    use super::Grant;

    /// `[remote] enabled`'s four cases, which `ConfigGrant` decided before this
    /// type existed. Its built-in is `false`: unset is not a grant.
    #[test]
    fn a_switch_denies_from_the_project_and_grants_only_from_the_user() {
        let g = |project, user| Grant::from_layers(project, user, false);

        // A project denial is effective, and is reported even with no user layer
        // — the report is the point, since the built-in already denies.
        assert_eq!(g(Some(false), None).as_effective(), Some(false));
        assert_eq!(g(Some(false), Some(true)).as_effective(), Some(false));
        assert!(g(Some(false), Some(true)).project_narrowed());

        // A project grant is discarded, and says so rather than vanishing.
        assert_eq!(g(Some(true), None).as_effective(), None);
        assert!(g(Some(true), None).project_widening_ignored());

        // Silence stays silent: "nobody set this" is not "set to the default".
        assert_eq!(g(None, None).as_effective(), None);
        assert_eq!(g(None, Some(true)).as_effective(), Some(true));
    }

    /// The same rule at a numeric width, which is what made this type shared
    /// rather than a second copy of the rule.
    #[test]
    fn a_bound_may_be_lowered_by_the_project_and_raised_only_by_the_user() {
        let g = |project, user| Grant::from_layers(project, user, 32_768_usize);

        // The user raises; the project cannot.
        assert_eq!(g(None, Some(131_072)).resolved(), 131_072);
        assert_eq!(g(Some(131_072), None).resolved(), 32_768);
        assert!(g(Some(131_072), None).project_widening_ignored());

        // The project lowers — including below a raise the user asked for, which
        // is the case the inversion exists for: the committed file tightens what
        // the machine owner loosened.
        assert_eq!(g(Some(16_384), None).resolved(), 16_384);
        assert_eq!(g(Some(65_536), Some(131_072)).resolved(), 65_536);
        assert!(g(Some(65_536), Some(131_072)).project_narrowed());

        // Neither layer: the built-in, reported as unset.
        assert_eq!(g(None, None).as_effective(), None);
        assert_eq!(g(None, None).resolved(), 32_768);
    }

    /// A project naming exactly the baseline widens nothing, so it applies —
    /// and must not be misreported as an overruled grant.
    #[test]
    fn a_project_restating_the_baseline_is_not_an_ignored_grant() {
        let at_default = Grant::from_layers(Some(32_768_usize), None, 32_768);
        assert_eq!(at_default.as_effective(), Some(32_768));
        assert!(!at_default.project_widening_ignored());
        assert!(!at_default.project_narrowed());
    }
}

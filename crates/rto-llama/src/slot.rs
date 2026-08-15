//! A process-wide slot for a lazily built **native** resource that must be
//! destroyed before the process exits.
//!
//! Two things in Roteiro are process-global and own native llama.cpp/ggml state:
//! the media extractors' engines (`rto-graph`'s `extract`, one per modality) and
//! — one level down — the single llama.cpp backend (`crate::backend`) every
//! engine borrows. Holding either in a `static` `OnceLock` is fatal on the Metal
//! backend: Rust never drops `static`s, so a loaded model's GPU buffers stay
//! registered in ggml-metal's device residency set. When libc's C++ finalizers
//! tear down ggml-metal's global device vector at `exit()`,
//! `ggml_metal_rsets_free` asserts that set is empty, finds it is not, and
//! `abort()`s — turning a completely successful run into SIGABRT (exit 134).
//!
//! [`EngineSlot`] keeps the build-once-and-reuse behaviour but makes the resource
//! **droppable at a point we choose**:
//!
//! - callers get an [`Arc`] handle, and once the resource exists the slot's lock
//!   is held only long enough to clone one out — generation never serialises
//!   behind it. The one-time build itself *does* run under the lock, deliberately;
//!   see [`EngineSlot::get_or_init`];
//! - [`EngineSlot::release`] drops the slot's own handle at a deterministic
//!   moment (the end of a CLI run), long before any C++ finalizer runs;
//! - [`EngineSlot::release_if_unshared`] is the stricter variant the shared
//!   backend needs: it refuses to release while anyone still holds a handle, so
//!   "engines are torn down before the backend they borrow" is enforced by
//!   ownership rather than by call ordering.
//!
//! Release is idempotent, safe on a slot that was never initialised, and resets
//! the slot to its pristine state — a later call re-initialises, which is what
//! makes the mechanism testable without a GPU or an installed model.
//!
//! This module is compiled unconditionally — it needs neither the `llama` feature
//! nor a C/C++ toolchain — so its unit tests cover the teardown mechanism on
//! every platform CI runs on; only the media features and the shared backend
//! actually instantiate a slot.

use std::sync::{Arc, Mutex, PoisonError};

/// A lazily initialised, explicitly releasable holder for one native resource.
///
/// See the [module docs](self) for why the resource may not simply live in a
/// `static OnceLock`.
pub struct EngineSlot<T> {
    state: Mutex<SlotState<T>>,
}

/// What a slot currently holds.
enum SlotState<T> {
    /// Initialisation has not been attempted yet.
    Uninit,
    /// Initialisation was attempted and the resource is unavailable (model not
    /// installed, or the backend failed to start). Cached so the next blob does
    /// not retry the probe. Only [`EngineSlot::get_or_init`] writes this state.
    Absent,
    /// A live resource, shared with every in-flight caller.
    Ready(Arc<T>),
}

impl<T> Default for EngineSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> EngineSlot<T> {
    /// An empty slot. `const` so a slot can be a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Uninit),
        }
    }

    /// The slot's resource, building it with `init` on first use.
    ///
    /// `init` runs at most once per initialisation cycle; a `None` from it means
    /// "unavailable" and is remembered, so a missing model costs one probe per
    /// process rather than one per blob. The returned [`Arc`] keeps the resource
    /// alive for the caller even if [`EngineSlot::release`] runs meanwhile.
    ///
    /// **The lock is held across `init`**, which is what makes "at most once"
    /// true rather than merely likely (pinned by
    /// `tests::concurrent_callers_build_one_engine`). A caller arriving mid-build
    /// therefore waits for that resource instead of starting a second load of the
    /// same multi-gigabyte model — the intended trade: it happens once per
    /// process, and a duplicate engine would mean a second set of GPU buffers to
    /// account for. Every call after that takes the lock only long enough to clone
    /// the [`Arc`], so inference never serialises behind the slot.
    pub fn get_or_init(&self, init: impl FnOnce() -> Option<T>) -> Option<Arc<T>> {
        // A poisoned lock only means some other caller's `init` panicked; the
        // slot itself is still consistent (the panic unwinds before any state is
        // written), so recover rather than cascade the panic into extraction.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            SlotState::Ready(engine) => return Some(Arc::clone(engine)),
            SlotState::Absent => return None,
            SlotState::Uninit => {}
        }
        let Some(engine) = init() else {
            // Remember the miss: a missing model then costs one probe per process,
            // not one per blob.
            *state = SlotState::Absent;
            return None;
        };
        let engine = Arc::new(engine);
        *state = SlotState::Ready(Arc::clone(&engine));
        Some(engine)
    }

    /// The slot's resource, building it with a **fallible** `init` on first use.
    ///
    /// The single-init guarantee is [`EngineSlot::get_or_init`]'s: the lock is
    /// held across `init`, so concurrent first-callers resolve to exactly one
    /// initialisation and the rest wait for its result. This is what the shared
    /// llama.cpp backend (`crate::backend`) uses — a backend that fails to start is
    /// an error the caller must see, not an "unavailable" to be swallowed.
    ///
    /// A failure leaves the slot **uninitialised**, so a later caller retries.
    /// Errors are deliberately not memoised the way [`SlotState::Absent`] memoises
    /// a missing model: an error value cannot be cloned for the callers that did
    /// not run `init`, so the honest choice is to let each of them attempt it and
    /// report its own failure.
    ///
    /// # Errors
    /// Returns whatever `init` returned, unchanged.
    pub fn get_or_try_init<E>(&self, init: impl FnOnce() -> Result<T, E>) -> Result<Arc<T>, E> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            SlotState::Ready(engine) => return Ok(Arc::clone(engine)),
            // `Absent` is only ever written by `get_or_init`, and a given slot is
            // driven by one of the two styles, so this is unreachable in practice.
            // Retrying is the conservative reading of it either way: a fallible
            // init reports to its own caller rather than inheriting a verdict
            // recorded by an infallible one.
            SlotState::Uninit | SlotState::Absent => {}
        }
        let engine = Arc::new(init()?);
        *state = SlotState::Ready(Arc::clone(&engine));
        Ok(engine)
    }

    /// Drop the slot's handle on its resource and reset it to uninitialised.
    ///
    /// Returns whether a live resource was released — `false` when the slot was
    /// never initialised or the resource was unavailable, so calling this on every
    /// exit path is free. Idempotent: a second call releases nothing.
    ///
    /// If a caller still holds an [`Arc`] from [`EngineSlot::get_or_init`], the
    /// resource outlives this call and dies with that handle instead; release is
    /// therefore called once work is finished, not to interrupt it. When that is
    /// not good enough — when re-initialising underneath a live handle would be
    /// *wrong*, as it is for the process-global backend — use
    /// [`EngineSlot::release_if_unshared`].
    pub fn release(&self) -> bool {
        // Take the old state out under the lock, but let it *drop* after the
        // guard: freeing a multi-gigabyte model should not hold the slot.
        let previous = std::mem::replace(
            &mut *self.state.lock().unwrap_or_else(PoisonError::into_inner),
            SlotState::Uninit,
        );
        matches!(previous, SlotState::Ready(_))
    }

    /// Release the resource **only if the slot is its last owner**, and destroy it
    /// there and then, under the slot's lock.
    ///
    /// Returns whether it was destroyed. `false` means either that nothing was
    /// resident or that somebody still holds a handle — in which case the slot
    /// keeps its own handle too, so the resource stays reachable and a later
    /// caller gets *that* resource rather than building a second one.
    ///
    /// This is the variant a process-global native singleton needs, and it is what
    /// makes "engines are released before the backend they borrow" a property of
    /// the type system rather than of call ordering: while any `LlamaEngine` is
    /// alive it holds an `Arc` on the backend, and this call is then a no-op.
    /// Destroying under the lock (unlike [`EngineSlot::release`], which drops
    /// after it) is equally deliberate: `llama_backend_free` must not race a
    /// concurrent `llama_backend_init` from a caller that has just observed the
    /// slot empty.
    pub fn release_if_unshared(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let SlotState::Ready(engine) = std::mem::replace(&mut *state, SlotState::Uninit) else {
            return false;
        };
        match Arc::try_unwrap(engine) {
            // Sole owner: destroy it here, while the slot is locked and already
            // reset, so no caller can observe the gap.
            Ok(engine) => {
                drop(engine);
                true
            }
            // Still borrowed — put it back. The borrower keeps working, and the
            // resource it borrowed is still the one the next caller will get.
            Err(engine) => {
                *state = SlotState::Ready(engine);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EngineSlot;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stand-in for a native engine: counts its own drops, so a test can prove
    /// teardown ran exactly once without a GPU or an installed model.
    struct Payload(Arc<AtomicUsize>);

    impl Drop for Payload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A fresh slot plus the drop counter its payloads report to.
    fn slot() -> (EngineSlot<Payload>, Arc<AtomicUsize>) {
        (EngineSlot::new(), Arc::new(AtomicUsize::new(0)))
    }

    #[test]
    fn initialises_once_and_reuses() {
        let (slot, drops) = slot();
        let inits = AtomicUsize::new(0);
        let build = || {
            inits.fetch_add(1, Ordering::SeqCst);
            Some(Payload(Arc::clone(&drops)))
        };

        assert!(slot.get_or_init(build).is_some());
        assert!(slot.get_or_init(build).is_some());
        assert!(slot.get_or_init(build).is_some());

        assert_eq!(inits.load(Ordering::SeqCst), 1, "engine built once per run");
        assert_eq!(drops.load(Ordering::SeqCst), 0, "engine still resident");
    }

    #[test]
    fn release_drops_the_engine_exactly_once_and_is_idempotent() {
        let (slot, drops) = slot();
        assert!(
            slot.get_or_init(|| Some(Payload(Arc::clone(&drops))))
                .is_some()
        );

        assert!(slot.release(), "first release reports the live engine");
        assert_eq!(drops.load(Ordering::SeqCst), 1, "engine dropped on release");

        assert!(!slot.release(), "second release has nothing to drop");
        assert!(!slot.release());
        assert_eq!(drops.load(Ordering::SeqCst), 1, "no double free");
    }

    #[test]
    fn release_is_safe_when_never_initialised() {
        let (slot, drops) = slot();
        assert!(!slot.release(), "an untouched slot releases nothing");
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unavailable_engine_is_remembered_and_releases_nothing() {
        let slot: EngineSlot<Payload> = EngineSlot::new();
        let inits = AtomicUsize::new(0);
        let build = || {
            inits.fetch_add(1, Ordering::SeqCst);
            None
        };

        assert!(slot.get_or_init(build).is_none());
        assert!(slot.get_or_init(build).is_none());
        assert_eq!(inits.load(Ordering::SeqCst), 1, "missing model probed once");
        assert!(!slot.release(), "nothing was resident to release");
    }

    #[test]
    fn release_resets_the_slot_for_a_later_run() {
        let (slot, drops) = slot();
        assert!(
            slot.get_or_init(|| Some(Payload(Arc::clone(&drops))))
                .is_some()
        );
        assert!(slot.release());

        assert!(
            slot.get_or_init(|| Some(Payload(Arc::clone(&drops))))
                .is_some(),
            "a released slot re-initialises on demand"
        );
        assert!(slot.release());
        assert_eq!(drops.load(Ordering::SeqCst), 2, "each engine dropped once");
    }

    #[test]
    fn an_outstanding_handle_keeps_the_engine_alive_past_release() {
        let (slot, drops) = slot();
        let held = slot
            .get_or_init(|| Some(Payload(Arc::clone(&drops))))
            .expect("engine built");

        assert!(slot.release());
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "an in-flight caller is never freed underneath"
        );

        drop(held);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "freed with the last handle"
        );
    }

    #[test]
    fn concurrent_callers_build_one_engine() {
        let (slot, drops) = slot();
        let inits = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let engine = slot.get_or_init(|| {
                        inits.fetch_add(1, Ordering::SeqCst);
                        Some(Payload(Arc::clone(&drops)))
                    });
                    assert!(engine.is_some());
                });
            }
        });

        assert_eq!(
            inits.load(Ordering::SeqCst),
            1,
            "one engine for all threads"
        );
        assert!(slot.release());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn try_init_builds_once_and_reuses() {
        let (slot, drops) = slot();
        let inits = AtomicUsize::new(0);
        let build = || -> Result<Payload, &'static str> {
            inits.fetch_add(1, Ordering::SeqCst);
            Ok(Payload(Arc::clone(&drops)))
        };

        assert!(slot.get_or_try_init(build).is_ok());
        assert!(slot.get_or_try_init(build).is_ok());
        assert_eq!(inits.load(Ordering::SeqCst), 1, "backend started once");
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn try_init_surfaces_the_error_and_stays_retryable() {
        // A failed start is reported to *its* caller and leaves the slot pristine:
        // unlike a missing model, a backend that failed to initialise is not a
        // verdict one caller may record on behalf of all the others.
        let (slot, drops) = slot();
        assert_eq!(
            slot.get_or_try_init(|| Err::<Payload, _>("boom")).err(),
            Some("boom")
        );
        assert!(!slot.release(), "a failed init leaves nothing resident");

        assert!(
            slot.get_or_try_init(|| Ok::<_, &str>(Payload(Arc::clone(&drops))))
                .is_ok(),
            "a later caller retries rather than inheriting the failure"
        );
        assert!(slot.release());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_try_init_callers_initialise_once() {
        // The single-init guarantee the shared llama.cpp backend depends on:
        // llama.cpp's `LlamaBackend::init` errors if a live backend already
        // exists, so "exactly one winner, everyone else gets that one" has to hold
        // under a race, not merely usually.
        let (slot, drops) = slot();
        let inits = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    let got = slot.get_or_try_init(|| -> Result<Payload, &'static str> {
                        inits.fetch_add(1, Ordering::SeqCst);
                        Ok(Payload(Arc::clone(&drops)))
                    });
                    assert!(got.is_ok());
                });
            }
        });

        assert_eq!(
            inits.load(Ordering::SeqCst),
            1,
            "one backend for all threads"
        );
        assert!(slot.release_if_unshared());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn release_if_unshared_refuses_while_a_handle_is_out() {
        let (slot, drops) = slot();
        let held = slot
            .get_or_try_init(|| Ok::<_, &str>(Payload(Arc::clone(&drops))))
            .expect("built");

        assert!(
            !slot.release_if_unshared(),
            "a borrowed resource must not be destroyed"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0, "nothing was freed");

        // ...and the slot kept its own handle, so the borrower and a fresh caller
        // still see the *same* resource — no second initialisation underneath.
        let again = slot
            .get_or_try_init(|| -> Result<Payload, &'static str> {
                panic!("must not re-initialise while borrowed")
            })
            .expect("still resident");
        assert!(Arc::ptr_eq(&held, &again));

        drop(again);
        drop(held);
        assert!(
            slot.release_if_unshared(),
            "sole owner now: release succeeds"
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!slot.release_if_unshared(), "idempotent");
    }

    #[test]
    fn release_if_unshared_is_safe_when_never_initialised() {
        let (slot, drops) = slot();
        assert!(!slot.release_if_unshared());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }
}

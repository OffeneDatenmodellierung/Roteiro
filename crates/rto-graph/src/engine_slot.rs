//! A process-wide slot for a lazily built **native** engine that must be
//! destroyed before the process exits.
//!
//! The media extractors ([`crate::extract`]) load a llama.cpp engine once per
//! process and reuse it across blobs. Holding that engine in a `static`
//! `OnceLock` is fatal on the Metal backend: Rust never drops `static`s, so the
//! engine's GPU buffers stay registered in ggml-metal's device residency set.
//! When libc's C++ finalizers tear down ggml-metal's global device vector at
//! `exit()`, `ggml_metal_rsets_free` asserts that set is empty, finds it is not,
//! and `abort()`s — turning a completely successful run into SIGABRT (exit 134).
//!
//! [`EngineSlot`] keeps the load-once-and-reuse behaviour but makes the engine
//! **droppable at a point we choose**:
//!
//! - callers get an [`Arc`] handle, and once the engine exists the slot's lock is
//!   held only long enough to clone one out — generation never serialises behind
//!   it. The one-time build itself *does* run under the lock, deliberately; see
//!   [`EngineSlot::get_or_init`];
//! - [`EngineSlot::release`] drops the slot's own handle at a deterministic
//!   moment (the end of a CLI run), long before any C++ finalizer runs.
//!
//! Release is idempotent, safe on a slot that was never initialised, and resets
//! the slot to its pristine state — a later call re-initialises, which is what
//! makes the mechanism testable without a GPU or an installed model.
//!
//! This module is compiled unconditionally (even in the default, llama-free
//! build) so its unit tests cover the teardown mechanism on every platform CI
//! runs on; only the media features actually instantiate a slot.

use std::sync::{Arc, Mutex, PoisonError};

/// A lazily initialised, explicitly releasable holder for one native engine.
///
/// See the [module docs](self) for why the engine may not simply live in a
/// `static OnceLock`.
// Without a media feature nothing in the library instantiates a slot; the type
// is still compiled (and unit-tested) so the teardown mechanism is covered by
// the default CI build, which has no C/C++ toolchain.
#[cfg_attr(
    not(any(feature = "image-vision", feature = "audio-transcribe")),
    allow(dead_code)
)]
pub(crate) struct EngineSlot<T> {
    state: Mutex<SlotState<T>>,
}

/// What a slot currently holds.
enum SlotState<T> {
    /// Initialisation has not been attempted yet.
    Uninit,
    /// Initialisation was attempted and the engine is unavailable (model not
    /// installed, or the backend failed to start). Cached so the next blob does
    /// not retry the probe.
    Absent,
    /// A live engine, shared with every in-flight caller.
    Ready(Arc<T>),
}

#[cfg_attr(
    not(any(feature = "image-vision", feature = "audio-transcribe")),
    allow(dead_code)
)]
impl<T> EngineSlot<T> {
    /// An empty slot. `const` so a slot can be a `static`.
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Uninit),
        }
    }

    /// The slot's engine, building it with `init` on first use.
    ///
    /// `init` runs at most once per initialisation cycle; a `None` from it means
    /// "unavailable" and is remembered, so a missing model costs one probe per
    /// process rather than one per blob. The returned [`Arc`] keeps the engine
    /// alive for the caller even if [`EngineSlot::release`] runs meanwhile.
    ///
    /// **The lock is held across `init`**, which is what makes "at most once"
    /// true rather than merely likely (pinned by
    /// `tests::concurrent_callers_build_one_engine`). A caller arriving mid-build
    /// therefore waits for that engine instead of starting a second load of the
    /// same multi-gigabyte model — the intended trade: it happens once per
    /// process, and a duplicate engine would mean a second set of GPU buffers to
    /// account for. Every call after that takes the lock only long enough to clone
    /// the [`Arc`], so inference never serialises behind the slot.
    pub(crate) fn get_or_init(&self, init: impl FnOnce() -> Option<T>) -> Option<Arc<T>> {
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

    /// Drop the slot's handle on its engine and reset it to uninitialised.
    ///
    /// Returns whether a live engine was released — `false` when the slot was
    /// never initialised or the engine was unavailable, so calling this on every
    /// exit path is free. Idempotent: a second call releases nothing.
    ///
    /// If a caller still holds an [`Arc`] from [`EngineSlot::get_or_init`], the
    /// engine outlives this call and dies with that handle instead; release is
    /// therefore called once work is finished, not to interrupt it.
    pub(crate) fn release(&self) -> bool {
        // Take the old state out under the lock, but let it *drop* after the
        // guard: freeing a multi-gigabyte model should not hold the slot.
        let previous = std::mem::replace(
            &mut *self.state.lock().unwrap_or_else(PoisonError::into_inner),
            SlotState::Uninit,
        );
        matches!(previous, SlotState::Ready(_))
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
}

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
//! [`KeyedSlot`] is the same discipline for a resource that exists in **several
//! distinct instances** rather than one: the multimodal projectors (issue #301),
//! where a process can legitimately hold a vision one and an audio one at the
//! same time and handing a caller the wrong one would be a correctness bug, not
//! merely a waste. It is one slot per key, built once per key, released together.
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

/// A lazily initialised, explicitly releasable holder for **one native resource
/// per key**.
///
/// [`EngineSlot`]'s sibling for the case where "the resource" is not a singleton:
/// Roteiro's multimodal projectors (issue #301). A process may hold a vision
/// projector and an audio projector at once, and they are not
/// interchangeable — `mtmd_init_from_file` loads whichever modality the `mmproj`
/// on disk implements, so a single unkeyed slot would hand an audio caller the
/// vision projector and fail the request (or, worse, answer it wrongly). The key
/// is therefore part of the mechanism rather than a caller's discipline; the
/// per-key isolation is pinned by `tests::each_key_gets_its_own_resource`.
///
/// The keeping-it-alive and releasing-it rules are [`EngineSlot`]'s, unchanged:
/// callers get an [`Arc`] handle, the lock is held across a build so "once per
/// key" is true rather than likely, [`KeyedSlot::release`] drops the slot's own
/// handles at a moment of our choosing, and an outstanding handle keeps its
/// resource alive past that release rather than being freed underneath.
///
/// Entries live in a `Vec` and are found by linear scan. That is deliberate: a
/// slot holds one or two projectors, so a map's hashing would cost more than the
/// scan, and `K` then needs only [`Eq`] — no `Hash` bound on a key type
/// (a `PathBuf`) chosen for what it *identifies*, not for how it hashes.
pub struct KeyedSlot<K, T> {
    entries: Mutex<Vec<(K, Arc<T>)>>,
}

impl<K, T> Default for KeyedSlot<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, T> KeyedSlot<K, T> {
    /// An empty slot. `const` so a slot can be a `static`. Unbounded in `K`, so an
    /// empty slot costs no `Eq` — only looking a key *up* does.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Drop the slot's handle on **every** resource it holds and reset it to
    /// empty.
    ///
    /// Returns how many were released — `0` when nothing was ever built, so
    /// calling this on every exit path is free. Idempotent: a second call releases
    /// nothing.
    ///
    /// A resource a caller still holds an [`Arc`] on outlives this call and dies
    /// with that handle instead, exactly as in [`EngineSlot::release`]: release
    /// ends a slot's ownership, it does not interrupt work in flight.
    pub fn release(&self) -> usize {
        // Take the entries out under the lock but let them *drop* after the guard:
        // freeing several hundred megabytes of projector should not hold the slot.
        let previous =
            std::mem::take(&mut *self.entries.lock().unwrap_or_else(PoisonError::into_inner));
        previous.len()
    }
}

impl<K: Eq, T> KeyedSlot<K, T> {
    /// The resource stored under `key`, building it with a **fallible** `init` on
    /// first use and reusing it afterwards.
    ///
    /// `init` runs at most once per key per initialisation cycle. As in
    /// [`EngineSlot::get_or_try_init`] the lock is held across it, so concurrent
    /// first-callers for the same key resolve to exactly one initialisation. A
    /// caller for a *different* key waits behind that build too — the honest cost
    /// of the simpler mechanism, and a small one: each key is built once per
    /// process, so the wait is bounded by the total number of keys, and Roteiro's
    /// two projectors live in two different engines' slots anyway, so in practice
    /// they never contend.
    ///
    /// A failure leaves the key **uninitialised** so a later caller retries; as in
    /// the unkeyed slot, an error cannot be cloned for callers that did not run
    /// `init`, so each reports its own.
    ///
    /// # Errors
    /// Returns whatever `init` returned, unchanged.
    pub fn get_or_try_init<E>(
        &self,
        key: K,
        init: impl FnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E> {
        // As in `EngineSlot`: a poisoned lock only means another caller's `init`
        // panicked, and the panic unwinds before anything is written, so the
        // entries are still consistent. Recover rather than cascade.
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, resource)) = entries.iter().find(|(k, _)| *k == key) {
            return Ok(Arc::clone(resource));
        }
        let resource = Arc::new(init()?);
        entries.push((key, Arc::clone(&resource)));
        Ok(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineSlot, KeyedSlot};
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

    /// The keyed slot the multimodal projectors live in (issue #301). Same
    /// payload, same drop counting, no GPU and no model — so Ubuntu CI runs all
    /// of this on the default feature set.
    mod keyed {
        use super::{KeyedSlot, Payload};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// A fresh keyed slot plus the drop counter its payloads report to.
        fn slot() -> (KeyedSlot<&'static str, Payload>, Arc<AtomicUsize>) {
            (KeyedSlot::new(), Arc::new(AtomicUsize::new(0)))
        }

        #[test]
        fn initialises_once_per_key_and_reuses() {
            // The whole point of #301: N blobs through one projector must build it
            // once, not N times.
            let (slot, drops) = slot();
            let inits = AtomicUsize::new(0);
            let mut built = Vec::new();
            for _ in 0..5 {
                built.push(
                    slot.get_or_try_init("vision", || -> Result<_, &str> {
                        inits.fetch_add(1, Ordering::SeqCst);
                        Ok(Payload(Arc::clone(&drops)))
                    })
                    .expect("built"),
                );
            }

            assert_eq!(inits.load(Ordering::SeqCst), 1, "one projector for 5 blobs");
            assert!(
                built.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])),
                "every caller got the same resource, not a look-alike copy"
            );
            assert_eq!(drops.load(Ordering::SeqCst), 0, "still resident");
        }

        #[test]
        fn each_key_gets_its_own_resource() {
            // The constraint that makes this slot *keyed*: a vision projector and
            // an audio projector coexist in one process (issue #298), and an audio
            // caller handed the vision one would fail — or silently answer with the
            // wrong modality. Two keys, two resources, never shared.
            let (slot, drops) = slot();
            let inits = AtomicUsize::new(0);
            let build = || -> Result<Payload, &'static str> {
                inits.fetch_add(1, Ordering::SeqCst);
                Ok(Payload(Arc::clone(&drops)))
            };

            let vision = slot.get_or_try_init("vision", build).expect("built");
            let audio = slot.get_or_try_init("audio", build).expect("built");
            assert_eq!(inits.load(Ordering::SeqCst), 2, "one build per key");
            assert!(
                !Arc::ptr_eq(&vision, &audio),
                "distinct keys must not share one resource"
            );

            // ...and each key keeps returning *its* resource, not the other's.
            assert!(Arc::ptr_eq(
                &vision,
                &slot
                    .get_or_try_init("vision", || -> Result<Payload, &str> {
                        panic!("must not rebuild a resident key")
                    })
                    .expect("resident")
            ));
            assert!(Arc::ptr_eq(
                &audio,
                &slot
                    .get_or_try_init("audio", || -> Result<Payload, &str> {
                        panic!("must not rebuild a resident key")
                    })
                    .expect("resident")
            ));
            assert_eq!(inits.load(Ordering::SeqCst), 2, "no rebuilds");
        }

        #[test]
        fn release_drops_every_key_exactly_once_and_is_idempotent() {
            let (slot, drops) = slot();
            for key in ["vision", "audio"] {
                slot.get_or_try_init(key, || -> Result<_, &str> {
                    Ok(Payload(Arc::clone(&drops)))
                })
                .expect("built");
            }

            assert_eq!(slot.release(), 2, "both projectors reported released");
            assert_eq!(drops.load(Ordering::SeqCst), 2, "both actually dropped");
            assert_eq!(slot.release(), 0, "a second release has nothing to drop");
            assert_eq!(drops.load(Ordering::SeqCst), 2, "no double free");
        }

        #[test]
        fn release_is_safe_when_never_initialised() {
            // Every teardown path calls this unconditionally, including on a build
            // that never touched a media blob.
            let (slot, drops) = slot();
            assert_eq!(slot.release(), 0);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }

        #[test]
        fn an_outstanding_handle_keeps_its_resource_alive_past_release() {
            // Teardown ordering, expressed as ownership: releasing the slot while a
            // request is mid-flight must not free the projector under it. This is
            // the property that makes "projectors die before the engine, which dies
            // before the backend" hold even when release lands mid-run.
            let (slot, drops) = slot();
            let held = slot
                .get_or_try_init("audio", || -> Result<_, &str> {
                    Ok(Payload(Arc::clone(&drops)))
                })
                .expect("built");

            assert_eq!(slot.release(), 1, "the slot gave up its own handle");
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
        fn release_resets_the_slot_for_a_later_run() {
            let (slot, drops) = slot();
            slot.get_or_try_init("vision", || -> Result<_, &str> {
                Ok(Payload(Arc::clone(&drops)))
            })
            .expect("built");
            assert_eq!(slot.release(), 1);

            assert!(
                slot.get_or_try_init("vision", || -> Result<_, &str> {
                    Ok(Payload(Arc::clone(&drops)))
                })
                .is_ok(),
                "a released slot re-initialises on demand"
            );
            assert_eq!(slot.release(), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 2, "each dropped once");
        }

        #[test]
        fn a_failed_init_is_not_cached_and_the_key_stays_retryable() {
            // A projector that failed to load (a truncated `mmproj`, say) is an
            // error the *caller* must see; it is not a verdict recorded for every
            // later blob, and it must leave nothing resident to release.
            let (slot, drops) = slot();
            assert_eq!(
                slot.get_or_try_init("audio", || Err::<Payload, _>("boom"))
                    .err(),
                Some("boom")
            );
            assert_eq!(slot.release(), 0, "a failed init left nothing resident");

            assert!(
                slot.get_or_try_init("audio", || -> Result<_, &str> {
                    Ok(Payload(Arc::clone(&drops)))
                })
                .is_ok(),
                "a later caller retries rather than inheriting the failure"
            );
            assert_eq!(slot.release(), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn concurrent_callers_build_one_resource_per_key() {
            // "Once per key" has to hold under a race, not merely usually: two
            // threads that both loaded the same 715 MB projector would mean two
            // sets of GPU buffers to account for at teardown.
            let (slot, drops) = slot();
            let inits = AtomicUsize::new(0);

            std::thread::scope(|scope| {
                for i in 0..8 {
                    let slot = &slot;
                    let (inits, drops) = (&inits, &drops);
                    scope.spawn(move || {
                        let key = if i % 2 == 0 { "vision" } else { "audio" };
                        let got = slot.get_or_try_init(key, || -> Result<_, &str> {
                            inits.fetch_add(1, Ordering::SeqCst);
                            Ok(Payload(Arc::clone(drops)))
                        });
                        assert!(got.is_ok());
                    });
                }
            });

            assert_eq!(
                inits.load(Ordering::SeqCst),
                2,
                "eight callers, two keys, two resources"
            );
            assert_eq!(slot.release(), 2);
            assert_eq!(drops.load(Ordering::SeqCst), 2);
        }
    }
}

//! The one llama.cpp backend this process owns (issue #296), behind the `llama`
//! feature.
//!
//! llama.cpp's backend is a **process-global**, and `llama-cpp-2` models that
//! faithfully: [`LlamaBackend::init`] flips a global `AtomicBool` and returns
//! `BackendAlreadyInitialized` if a live backend already exists. Building an
//! engine per modality therefore worked only for whichever engine was built
//! first; every later one failed to construct, and because both media extractors
//! and the CLI's generative paths swallow that with `.ok()`, the second modality
//! was not loudly broken but **silently inert** — the worst shape for `roteiro
//! serve`, a long-lived process that may legitimately want a second modality
//! without a restart.
//!
//! So the backend is initialised **once per process** and shared: every
//! [`crate::llama::LlamaEngine`] holds an [`Arc`] handle to it rather than its own.
//!
//! **Why sharing is sound.** `LlamaBackend` is a field-less
//! (`pub struct LlamaBackend {}`) proof-of-initialisation token: it carries no
//! state, so it inherits `Send + Sync` automatically and an `Arc<LlamaBackend>`
//! needs no `unsafe impl` — which matters, because `unsafe_code = "forbid"` is
//! workspace policy and would have ruled the design out otherwise. The
//! `assert_send_sync` const below pins that as a compile error rather than as a
//! comment that could quietly go stale. Nothing about *use* is
//! serialised by the backend either: it is only ever passed as `&LlamaBackend` to
//! `LlamaModel::load_from_file` / `LlamaModel::new_context`, and llama.cpp's own
//! per-object rules are what [`crate::llama::LlamaEngine`] already honours with
//! its per-model `gen_lock`. Sharing one token changes none of that — before this
//! change two engines could not coexist at all, so no serialisation was lost.
//!
//! **Teardown ordering (issue #291 / #292).** The backend must be freed *after*
//! every engine that borrows it, and that is now enforced by ownership rather
//! than by call order: [`release_shared_backend`] is a no-op while any engine
//! still holds a handle (see [`EngineSlot::release_if_unshared`]), and an engine
//! cannot outlive the backend because it owns an `Arc` on it. `rto-graph`'s
//! `release_media_engines` releases its engines and then calls this, and
//! `roteiro`'s `main` holds a [`SharedBackendGuard`] declared *before* the media
//! guard, so the media guard drops first. On both paths the sequence is engines,
//! then backend — and if it ever were not, this call would simply decline.

use std::sync::Arc;

use llama_cpp_2::llama_backend::LlamaBackend;

use crate::slot::EngineSlot;

/// The process's single llama.cpp backend.
static SHARED_BACKEND: EngineSlot<LlamaBackend> = EngineSlot::new();

// Compile-time proof that `LlamaBackend` may be shared across threads, i.e. that
// `Arc<LlamaBackend>` is sound *without* an `unsafe impl` — which matters because
// `unsafe_code = "forbid"` would reject one. If a future `llama-cpp-2` gives the
// backend interior state and makes it `!Sync`, this stops compiling, so the
// design is revisited deliberately rather than silently going unsound.
const fn assert_send_sync<T: Send + Sync>() {}
const _: () = assert_send_sync::<LlamaBackend>();

/// A handle on the process's llama.cpp backend, starting it on first use.
///
/// Concurrent first callers resolve to exactly one `llama_backend_init`: the
/// slot's lock is held across initialisation, so the losers of the race wait and
/// receive the winner's backend rather than attempting a second init that
/// llama.cpp would reject.
///
/// # Errors
/// Returns the error from [`LlamaBackend::init`] if the backend fails to start.
/// A failure is not cached: the next engine construction tries again.
pub(crate) fn shared_backend() -> anyhow::Result<Arc<LlamaBackend>> {
    SHARED_BACKEND.get_or_try_init(|| LlamaBackend::init().map_err(anyhow::Error::from))
}

/// Free the process's llama.cpp backend, **if no engine still borrows it**.
///
/// Returns whether it was freed. `false` means either that no backend was ever
/// started or that an engine is still alive — in which case nothing happens and
/// the backend stays valid for that engine, which is the point: this cannot pull
/// the backend out from under a live [`crate::llama::LlamaEngine`], because the
/// engine holds an [`Arc`] on it.
///
/// Idempotent and cheap, so it is safe on every exit path. Callers release their
/// engines first; see the [module docs](self).
#[must_use]
pub fn release_shared_backend() -> bool {
    SHARED_BACKEND.release_if_unshared()
}

/// Ties the process's llama.cpp backend to a scope: dropping the guard runs
/// [`release_shared_backend`].
///
/// The sibling of `rto_graph::MediaEngineGuard`, one level down. `roteiro`'s
/// `main` holds one for the whole run, declared **before** the media guard so
/// that it drops **after** it — engines first, backend last. It covers the builds
/// `rto-graph` cannot see, such as a `serve`-only build where the only engine is
/// the server's.
///
/// `std::process::exit` skips destructors, so any path that exits that way must
/// call [`release_shared_backend`] itself first.
#[derive(Debug)]
pub struct SharedBackendGuard {
    // A private field keeps the guard un-constructible except through `hold`, so
    // it cannot be created (and dropped) by accident mid-run.
    _private: (),
}

impl SharedBackendGuard {
    /// Take ownership of the process's llama.cpp backend for this scope.
    #[must_use]
    pub const fn hold() -> Self {
        Self { _private: () }
    }
}

impl Drop for SharedBackendGuard {
    fn drop(&mut self) {
        // Whether a backend was resident is of no consequence here — the point is
        // that none is, from now on (unless an engine outlives this guard, in
        // which case declining is the correct answer).
        let _released = release_shared_backend();
    }
}

#[cfg(test)]
mod tests {
    use super::{SharedBackendGuard, release_shared_backend};

    /// Releasing a backend nobody started is a no-op, so every exit path may call
    /// it unconditionally — including the guard's own `Drop`. Deliberately does
    /// **not** start a backend: this is the crate's *unit* test binary, and the
    /// end-to-end sharing behaviour belongs to `tests/shared_backend.rs`, which
    /// owns the process-global for the whole of its own binary.
    #[test]
    fn releasing_is_safe_and_reports_nothing_when_unused() {
        drop(SharedBackendGuard::hold());
        assert!(!release_shared_backend());
    }
}

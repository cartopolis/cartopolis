//! `std::sync`, except that the browser's main thread never blocks on a lock.
//!
//! The web build shares one linear memory between the page and the worker pool
//! (`platform::workers`), so a lock the main thread takes may be held by a
//! worker at that moment. std's `Mutex` and `RwLock` spin briefly and then park
//! on `memory.atomic.wait32` — and a browser's main thread is not allowed to
//! block, so that instruction **traps** there: `RuntimeError: Atomics.wait
//! cannot be called in this context`, and the app is gone. It needs contention
//! that outlasts std's ~100 spins, i.e. a worker preempted inside its critical
//! section, so it is intermittent and depends on the device's load — which is
//! the shape of "the web build is unstable". std's own wasm allocator spins
//! rather than waits for exactly this reason.
//!
//! So on wasm the main thread spins on `try_lock` until it gets the lock, and a
//! worker (`mark_worker`) keeps std's parking behaviour, where it is legal and
//! cheaper. Everywhere else these are std's own types, re-exported unchanged.
//!
//! Use this module instead of `std::sync` throughout the client: it re-exports
//! everything else in `std::sync`, so `crate::platform::sync::{Arc, Mutex}` is a
//! drop-in. `OnceLock`/`LazyLock` initialisation can still wait, but only while
//! two threads race to *initialise* the same cell, not on every access.

pub use std::sync::*;

#[cfg(target_arch = "wasm32")]
pub use wasm::{mark_worker, Mutex, RwLock};

// The wrappers mirror std's API whether or not the client calls every method
// today, so a new `try_lock` or `into_inner` compiles on the web the same as it
// does natively instead of failing only on the lane nobody runs locally.
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
mod wasm {
    use std::cell::Cell;
    use std::sync::{
        LockResult, MutexGuard, RwLockReadGuard, RwLockWriteGuard, TryLockError, TryLockResult,
    };

    thread_local! {
        static MAY_BLOCK: Cell<bool> = const { Cell::new(false) };
    }

    /// Call first thing on a Web Worker: from here on, locks taken on this
    /// thread park instead of spinning.
    pub fn mark_worker() {
        MAY_BLOCK.with(|b| b.set(true));
    }

    fn may_block() -> bool {
        MAY_BLOCK.with(Cell::get)
    }

    /// Retry a `try_*` until it stops answering `WouldBlock`.
    fn spin<G>(mut attempt: impl FnMut() -> TryLockResult<G>) -> LockResult<G> {
        loop {
            match attempt() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(p)) => return Err(p),
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
            }
        }
    }

    #[derive(Default, Debug)]
    pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

    impl<T> Mutex<T> {
        pub const fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }

        pub fn into_inner(self) -> LockResult<T> {
            self.0.into_inner()
        }
    }

    impl<T: ?Sized> Mutex<T> {
        pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
            if may_block() {
                self.0.lock()
            } else {
                spin(|| self.0.try_lock())
            }
        }

        pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
            self.0.try_lock()
        }

        pub fn get_mut(&mut self) -> LockResult<&mut T> {
            self.0.get_mut()
        }
    }

    impl<T> From<T> for Mutex<T> {
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    #[derive(Default, Debug)]
    pub struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

    impl<T> RwLock<T> {
        pub const fn new(value: T) -> Self {
            Self(std::sync::RwLock::new(value))
        }

        pub fn into_inner(self) -> LockResult<T> {
            self.0.into_inner()
        }
    }

    impl<T: ?Sized> RwLock<T> {
        pub fn read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
            if may_block() {
                self.0.read()
            } else {
                spin(|| self.0.try_read())
            }
        }

        pub fn write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
            if may_block() {
                self.0.write()
            } else {
                spin(|| self.0.try_write())
            }
        }

        pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
            self.0.try_read()
        }

        pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
            self.0.try_write()
        }

        pub fn get_mut(&mut self) -> LockResult<&mut T> {
            self.0.get_mut()
        }
    }

    impl<T> From<T> for RwLock<T> {
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }
}

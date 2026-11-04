//! Keep the display from dimming while the app is in front.
//!
//! [`hold`] is called every frame by `frame_pacing::apply_screen_wakelock` with
//! the current answer; each arm is cheap when nothing changed.
//!
//! - **Android**: `platform::android::set_keep_screen_on`, a screen wake lock
//!   (not the window flag — that function says why).
//! - **Web**: the Screen Wake Lock API, `navigator.wakeLock.request("screen")`.
//!   The browser drops the lock by itself when the tab is hidden, so a held
//!   want re-asks once the page is visible again.
//! - **macOS**: an IOKit `PreventUserIdleDisplaySleep` power assertion, which
//!   `pmset -g assertions` lists under the app's name.
//! - **Linux and Windows**: nothing yet. Linux needs a D-Bus client
//!   (`org.freedesktop.ScreenSaver.Inhibit`) the build does not carry, and
//!   Windows' `SetThreadExecutionState` is per thread, so it needs a thread of
//!   its own; the screen times out there as it always did.

/// Hold the display awake (`true`) or let it go (`false`).
pub fn hold(on: bool) {
    imp::hold(on);
}

#[cfg(target_os = "android")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    static HELD: AtomicBool = AtomicBool::new(false);

    pub fn hold(on: bool) {
        if HELD.swap(on, Ordering::Relaxed) != on {
            crate::platform::android::set_keep_screen_on(on);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::cell::{Cell, RefCell};

    use wasm_bindgen::{JsCast, JsValue};

    /// After a refused request, wait this long before asking again: a browser
    /// in battery saver refuses every time, and asking each frame would only
    /// fill the console.
    const RETRY_MS: f64 = 10_000.0;

    thread_local! {
        /// The `WakeLockSentinel` the browser handed back, if any.
        static SENTINEL: RefCell<Option<JsValue>> = const { RefCell::new(None) };
        /// What the caller wants now; a request that resolves after the want
        /// went off releases what it got.
        static WANT: Cell<bool> = const { Cell::new(false) };
        static ASKING: Cell<bool> = const { Cell::new(false) };
        static RETRY_AT: Cell<f64> = const { Cell::new(0.0) };
        static UNSUPPORTED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn hold(on: bool) {
        WANT.set(on);
        if !on {
            if let Some(sentinel) = SENTINEL.take() {
                release(&sentinel);
            }
            return;
        }
        if ASKING.get() || UNSUPPORTED.get() || js_sys::Date::now() < RETRY_AT.get() {
            return;
        }
        // A sentinel the browser released (the tab was hidden) is dead: ask anew.
        let live = SENTINEL.with_borrow(|s| {
            s.as_ref().is_some_and(|s| {
                js_sys::Reflect::get(s, &"released".into())
                    .ok()
                    .and_then(|v| v.as_bool())
                    == Some(false)
            })
        });
        if live {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        // A hidden page is refused anyway; wait until it is shown.
        if window.document().is_none_or(|d| d.hidden()) {
            return;
        }
        let wake_lock = js_sys::Reflect::get(&window.navigator(), &"wakeLock".into())
            .unwrap_or(JsValue::UNDEFINED);
        let request = js_sys::Reflect::get(&wake_lock, &"request".into())
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
        let Some(request) = request else {
            tracing::info!("keep-screen-on: this browser has no screen wake lock");
            UNSUPPORTED.set(true);
            return;
        };
        let promise = match request.call1(&wake_lock, &"screen".into()) {
            Ok(p) => js_sys::Promise::from(p),
            Err(e) => {
                refused(&e);
                return;
            }
        };
        ASKING.set(true);
        wasm_bindgen_futures::spawn_local(async move {
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(sentinel) if WANT.get() => {
                    tracing::info!("keep-screen-on: held");
                    SENTINEL.set(Some(sentinel));
                }
                Ok(sentinel) => release(&sentinel),
                Err(e) => refused(&e),
            }
            ASKING.set(false);
        });
    }

    fn refused(e: &JsValue) {
        tracing::warn!(error = ?e, "keep-screen-on: refused");
        RETRY_AT.set(js_sys::Date::now() + RETRY_MS);
    }

    fn release(sentinel: &JsValue) {
        let release = js_sys::Reflect::get(sentinel, &"release".into())
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
        if let Some(release) = release {
            // The promise only says it is done; a failure leaves nothing to undo.
            let _ = release.call0(sentinel);
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_void};
    use std::sync::Mutex;

    type CFStringRef = *const c_void;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// `kIOPMAssertionLevelOn`.
    const LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            s: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    /// `None` while not wanted; `Some(None)` wanted and refused, so a refusal
    /// is not retried every frame; `Some(Some(id))` held. Process-wide, so any
    /// thread may take or release it.
    static ASSERTION: Mutex<Option<Option<u32>>> = Mutex::new(None);

    pub fn hold(on: bool) {
        let Ok(mut held) = ASSERTION.lock() else {
            return;
        };
        match (on, *held) {
            (true, None) => *held = Some(create()),
            (false, Some(id)) => {
                if let Some(id) = id {
                    // SAFETY: `id` came from a successful create and is released once.
                    unsafe { IOPMAssertionRelease(id) };
                    tracing::info!("keep-screen-on: released");
                }
                *held = None;
            }
            _ => {}
        }
    }

    fn create() -> Option<u32> {
        // SAFETY: both strings are NUL-terminated literals, each CFString is
        // released after the call, and `id` outlives it.
        unsafe {
            let kind = CFStringCreateWithCString(
                std::ptr::null(),
                c"PreventUserIdleDisplaySleep".as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            let name = CFStringCreateWithCString(
                std::ptr::null(),
                c"Cartopolis is on screen".as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            if kind.is_null() || name.is_null() {
                for s in [kind, name] {
                    if !s.is_null() {
                        CFRelease(s);
                    }
                }
                return None;
            }
            let mut id = 0u32;
            let status = IOPMAssertionCreateWithName(kind, LEVEL_ON, name, &mut id);
            CFRelease(kind);
            CFRelease(name);
            if status == 0 {
                tracing::info!("keep-screen-on: held");
                Some(id)
            } else {
                tracing::warn!(status, "keep-screen-on: assertion refused");
                None
            }
        }
    }
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32", target_os = "macos")))]
mod imp {
    pub fn hold(_on: bool) {}
}

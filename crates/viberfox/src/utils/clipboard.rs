//! System clipboard access (ADR-030 substitutable provider).
//!
//! Native uses `arboard` (X11/Wayland/AppKit/Win32); one long-lived `Clipboard`
//! instance, because on X11 the copied text is served by this process, so dropping
//! it would clear the selection. The web uses the async Clipboard API
//! (`navigator.clipboard`), which is why the two halves are not symmetric:
//! **copy works, paste does not**.
//!
//! Copying fits: `writeText` returns a promise, nothing waits on it, and the
//! browser grants the write off the user's click without a prompt. Reading does
//! not: `readText` is async *and* permission-gated (Chrome prompts, Firefox
//! refuses outright), while [`get_text`]'s callers are inside a frame and need an
//! answer now. The way the web actually pastes is the document's `paste` event,
//! which hands the text over synchronously — but that event only fires if the
//! keystroke reaches the page, and winit's canvas calls `preventDefault` on
//! keydown, so wiring it up means changing how input is claimed first. Until
//! then, the web build's paste path returns `None`, and the way to open a place
//! there is the `?place=` URL (see `systems::geo_nav::launch_place`).

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::sync::{Mutex, OnceLock};

    fn clipboard() -> &'static Mutex<Option<arboard::Clipboard>> {
        static CB: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
        CB.get_or_init(|| {
            Mutex::new(match arboard::Clipboard::new() {
                Ok(cb) => Some(cb),
                Err(e) => {
                    tracing::warn!(error = %e, "system clipboard unavailable");
                    None
                }
            })
        })
    }

    pub fn get_text() -> Option<String> {
        clipboard().lock().ok()?.as_mut()?.get_text().ok()
    }

    pub fn set_text(text: &str) {
        if let Ok(mut guard) = clipboard().lock() {
            if let Some(cb) = guard.as_mut() {
                if let Err(e) = cb.set_text(text.to_string()) {
                    tracing::warn!(error = %e, "clipboard copy failed");
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    /// See the module note: reading the clipboard on the web is asynchronous and
    /// permission-gated, so there is nothing to return by the time the caller
    /// needs it. Not a stub waiting on `readText` — that call is the wrong shape.
    pub fn get_text() -> Option<String> {
        None
    }

    pub fn set_text(text: &str) {
        let Some(window) = web_sys::window() else {
            return;
        };
        // `navigator.clipboard` exists only in a secure context; touching it on a
        // plain-http page throws a JS exception, which unwinds into wasm as an
        // abort rather than an error we could log. https and localhost qualify.
        if !window.is_secure_context() {
            tracing::warn!("clipboard write needs a secure context (https or localhost)");
            return;
        }
        // This runs from the frame loop, not from the click handler — the button
        // press is a Bevy input event we process on the next tick. That is fine:
        // `writeText` wants *transient user activation*, which outlives the click
        // by a few seconds, and a frame is well inside it. Copies not caused by a
        // click (a future scripted one) would be refused, which is the browser
        // working as intended.
        //
        // Fire and await: the promise is only interesting when it rejects (the
        // document not being focused is the usual reason), and a rejected promise
        // that nobody handles surfaces as a bare console error instead.
        let write = window.navigator().clipboard().write_text(text);
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = wasm_bindgen_futures::JsFuture::from(write).await {
                tracing::warn!(error = ?e, "clipboard copy failed");
            }
        });
    }
}

pub use imp::{get_text, set_text};

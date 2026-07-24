//! System clipboard access (ADR-030 substitutable provider).
//!
//! Native uses `arboard` (X11/Wayland/AppKit/Win32); one long-lived `Clipboard`
//! instance, because on X11 the copied text is served by this process, so dropping
//! it would clear the selection. The wasm build has no `arboard` (it doesn't build
//! for the browser) — it degrades to a no-op stub until an async `web-sys`
//! clipboard is wired up.

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
    // Stub until an async `web-sys` clipboard lands (ADR-033).
    pub fn get_text() -> Option<String> {
        None
    }
    pub fn set_text(_text: &str) {}
}

pub use imp::{get_text, set_text};

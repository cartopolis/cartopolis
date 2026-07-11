//! System clipboard access (X11/Wayland via `arboard`).
//!
//! One long-lived `Clipboard` instance: on X11 the copied text is served by
//! this process, so dropping the instance would clear the selection.

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

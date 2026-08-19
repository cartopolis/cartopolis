//! Runtime OS window icon, set through winit's `set_window_icon`.
//!
//! This is the per-window icon path that works on **Windows** and **Linux/X11**.
//! It is a deliberate no-op on **Wayland** (winit has no implementation — the
//! icon comes from the `.desktop` file `systems::desktop_integration` installs)
//! and on **macOS** (which has no per-window icons — the Dock icon comes from
//! the `.app` bundle's `.icns`, see the `[package.metadata.bundle]` config).
//! Calling it everywhere is harmless, so the one system covers all desktops.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy::winit::WinitWindows;

/// The app icon (an isometric cluster of map-clad towers with a place pin,
/// rendered from `assets/icon/cartopolis.svg`), embedded from the workspace
/// `assets/` dir. Shared with `systems::desktop_integration`, which installs it
/// into the XDG icon theme.
pub(crate) const ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icon/cartopolis.png"
));

/// Decode the embedded PNG and apply it as the primary window's icon, once.
///
/// Runs each `Update` frame until it succeeds (the winit window is created
/// lazily), then latches off. `Option<NonSend<_>>` because a scripted `--shot`
/// run has no `WinitWindows` (the `WinitPlugin` is disabled).
pub fn set_window_icon(
    windows: Option<NonSend<WinitWindows>>,
    primary: Query<Entity, With<PrimaryWindow>>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let Some(windows) = windows else { return };
    let Ok(entity) = primary.single() else { return };
    let Some(window) = windows.get_window(entity) else {
        return;
    };

    let rgba = match image::load_from_memory(ICON_PNG) {
        Ok(img) => img.into_rgba8(),
        Err(e) => {
            warn!("window icon: failed to decode embedded PNG: {e}");
            *done = true; // bad asset — retrying every frame won't help
            return;
        }
    };
    let (w, h) = rgba.dimensions();
    match winit::window::Icon::from_rgba(rgba.into_raw(), w, h) {
        Ok(icon) => window.set_window_icon(Some(icon)),
        Err(e) => warn!("window icon: rejected RGBA buffer: {e}"),
    }
    *done = true;
}

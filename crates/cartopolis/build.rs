//! Build script: embed the app icon into the Windows executable so Explorer, the
//! taskbar and pinned shortcuts show it (independent of the runtime
//! `set_window_icon` call in `systems::window_icon`).
//!
//! Gated on the *target* OS (`CARGO_CFG_TARGET_OS`), not the host `#[cfg(windows)]`,
//! so it also fires when **cross-compiling** to Windows from the Linux CI image
//! (winresource then drives `x86_64-w64-mingw32-windres`). On every non-Windows
//! target it's a no-op. Non-fatal: a missing resource compiler just omits the
//! icon rather than failing the build.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Path is relative to this crate's root (build.rs CWD).
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon/cartopolis.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cartopolis: failed to embed Windows icon: {e}");
        }
    }
}

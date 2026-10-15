//! Build script. Two jobs, both decided from the **target** rather than the host.
//!
//! 1. Embed the app icon into the Windows executable so Explorer, the taskbar and
//!    pinned shortcuts show it (independent of the runtime `set_window_icon` call
//!    in `systems::window_icon`).
//!
//!    Gated on `CARGO_CFG_TARGET_OS`, not the host `#[cfg(windows)]`, so it also
//!    fires when **cross-compiling** to Windows from the Linux CI image
//!    (winresource then drives `x86_64-w64-mingw32-windres`). On every non-Windows
//!    target it's a no-op. Non-fatal: a missing resource compiler just omits the
//!    icon rather than failing the build.
//!
//! 2. Set the `mcp` cfg on Unix desktops (Linux, macOS), so `#[cfg(mcp)]` says
//!    "this is a machine an IDE can drive" in one word instead of repeating
//!    `all(unix, not(target_os = "android"))` at every site. It matches the
//!    dependency table in `Cargo.toml`, which puts `cartopolis_mcp` under
//!    exactly that target predicate. Windows is not on it: the listener is a
//!    Unix domain socket (`docs/notes/driving-the-client-from-an-ide.md`).
//!
//!    Why not a cargo feature: Android and the web both build
//!    `--no-default-features`, so a default feature would be dropped there — which
//!    is the right outcome by accident, and the `audio` feature is this tree's
//!    standing evidence that "right by accident" becomes "silently missing" the
//!    day somebody adds a lane. The target predicate cannot be dropped.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // A comma-separated list (`unix`, or `unix,wasm` on emscripten-likes).
    let target_family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();

    if target_os == "windows" {
        // Path is relative to this crate's root (build.rs CWD).
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon/cartopolis.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cartopolis: failed to embed Windows icon: {e}");
        }
    }

    println!("cargo::rustc-check-cfg=cfg(mcp)");
    if target_family.split(',').any(|f| f == "unix") && target_os != "android" {
        println!("cargo::rustc-cfg=mcp");
    }

    // Naming any path switches cargo from "rerun on any change in this package"
    // to "rerun on these", so the script and the icon the first job embeds are
    // named. The basemap's stored-tile revision used to be hashed here from
    // `cartopolis_geo`'s sources; it is `cartopolis_geo::render_revision` now.
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=../../assets/icon/cartopolis.ico");
}

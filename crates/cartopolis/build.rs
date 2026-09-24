//! Build script. Three jobs; the first two are decided from the **target** rather
//! than the host, and the third is described at [`render_fingerprint`].
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
//! 2. Set the `mcp` cfg on desktop targets, so `#[cfg(mcp)]` says "this is a
//!    machine an IDE can drive" in one word instead of repeating
//!    `all(not(target_arch = "wasm32"), not(target_os = "android"))` at every
//!    site. It matches the dependency table in `Cargo.toml`, which puts `rmcp`
//!    and its HTTP stack under exactly that target predicate — the same shape
//!    `arboard` and `rfd` already use for "the desktop, and Android is not one".
//!
//!    Why not a cargo feature: Android and the web both build
//!    `--no-default-features`, so a default feature would be dropped there — which
//!    is the right outcome by accident, and the `audio` feature is this tree's
//!    standing evidence that "right by accident" becomes "silently missing" the
//!    day somebody adds a lane. The target predicate cannot be dropped.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if target_os == "windows" {
        // Path is relative to this crate's root (build.rs CWD).
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon/cartopolis.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cartopolis: failed to embed Windows icon: {e}");
        }
    }

    println!("cargo::rustc-check-cfg=cfg(mcp)");
    if target_arch != "wasm32" && target_os != "android" {
        println!("cargo::rustc-cfg=mcp");
    }

    render_fingerprint();
}

/// 3. Fingerprint the code a basemap tile's pixels come from, as
///    `CARTO_RENDER_FINGERPRINT`.
///
///    `tile_loader` keeps finished basemap tiles on disk, and a stored tile is
///    only right for the renderer that drew it. Everything that decides those
///    pixels lives in `cartopolis_geo` — the MVT raster, the palette, the mip
///    chain — plus the versions of the crates it draws with, so the fingerprint is
///    those sources and `Cargo.lock`. A version number someone has to remember to
///    bump would be the other way to do it, and the first restyle that forgot
///    would serve the old map from every warm cache until the entries aged out.
///
///    Over-inclusive on purpose: an edit anywhere in the crate empties the store
///    once, which costs one cold fill; missing an edit that changed the pixels
///    would cost a wrong map.
fn render_fingerprint() {
    use std::hash::{Hash, Hasher};

    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }

    let geo = std::path::Path::new("../geo/src");
    let lock = std::path::Path::new("../../Cargo.lock");
    // Naming any path switches cargo from "rerun on any change in this package" to
    // "rerun on these", so the script and the icon the first job embeds are named
    // too.
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=../../assets/icon/cartopolis.ico");
    println!("cargo::rerun-if-changed={}", geo.display());
    println!("cargo::rerun-if-changed={}", lock.display());

    let mut files = Vec::new();
    walk(geo, &mut files);
    // Sorted, so the hash is of the tree and not of `read_dir`'s order.
    files.sort();
    files.push(lock.to_path_buf());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &files {
        f.hash(&mut h);
        std::fs::read(f).unwrap_or_default().hash(&mut h);
    }
    println!(
        "cargo::rustc-env=CARTO_RENDER_FINGERPRINT={:016x}",
        h.finish()
    );
}

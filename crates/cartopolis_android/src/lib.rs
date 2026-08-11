//! The Android entrypoint: `libcartopolis_android.so`.
//!
//! Everything real is in `cartopolis`; this crate exists only to own the
//! `android_main` symbol and the `cdylib` link that carries it into the APK. See
//! `docs/notes/android-build.md` for why the app is packaged the way it is, and
//! `crates/cartopolis/android/build-apk.sh` for the packaging itself.
//!
//! Off Android this file is empty and the crate has no dependencies at all (they
//! are target-gated in `Cargo.toml`), so it costs a workspace build nothing.

/// Called by `android-activity`'s C glue once the framework has created the
/// activity, on a dedicated thread that is *not* the Java main thread.
///
/// Two things have to happen here and in this order. The `AndroidApp` handle is
/// the process's only route to the window, the event loop, the `AssetManager` and
/// the app-private data dir, and it arrives by argument — so it goes into
/// `bevy::android::ANDROID_APP` before anything else runs, because that static is
/// where `bevy_winit` reads it to build the event loop and where
/// `AndroidAssetReader` reads it for every single asset load. Then the ordinary
/// client starts, exactly as `src/main.rs` starts it on a desktop.
///
/// This is what `#[bevy_main]` would generate, written out. The macro insists on
/// being applied to a function called `main`, and a cdylib has none.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: bevy::android::android_activity::AndroidApp) {
    // `set` fails only if something already initialised it, which nothing can:
    // `android_main` is called once per process. Ignored rather than unwrapped so
    // a future second caller degrades instead of aborting across the FFI boundary.
    let _ = bevy::android::ANDROID_APP.set(app);
    cartopolis::run();
}

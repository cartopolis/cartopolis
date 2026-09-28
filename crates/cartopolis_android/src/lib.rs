//! The Android entrypoint: `libcartopolis_android.so`.
//!
//! Everything real is in `cartopolis`; this crate exists only to own the
//! `android_main` symbol and the `cdylib` link that carries it into the APK. See
//! `docs/notes/android-build.md` for why the app is packaged the way it is, and
//! `crates/cartopolis/android/build-apk.sh` for the packaging itself.
//!
//! Off Android this file is empty and the crate has no dependencies at all (they
//! are target-gated in `Cargo.toml`), so it costs a workspace build nothing.

/// Whether `android_main` has already run in this process — see its note.
#[cfg(target_os = "android")]
static ENTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
///
/// # The process must not outlive its activity
///
/// This used to say, in a comment, that `android_main` "is called once per
/// process". **It is not**, and the gap between that belief and the framework's
/// behaviour is a black screen with nothing in the log to explain it.
///
/// `ANativeActivity_onCreate` unconditionally spawns a fresh thread running
/// `android_main` for *every* activity instance (android-activity 0.6.1,
/// `native_activity/glue.rs`). A process routinely outlives its activity: the
/// back gesture writes `AppExit` (`lib.rs::handle_back_gesture`), `run()`
/// returns, and the glue answers that by calling `ANativeActivity_finish` — but
/// nothing kills the process, and Android keeps it as a cached one for as long as
/// it has memory to spare. Android may also destroy the activity on its own,
/// under memory pressure or with the developer option "Don't keep activities" on.
///
/// Reopening the app then lands in *this* function a second time, in a process
/// where:
///
/// - `bevy::android::ANDROID_APP` is a `OnceLock`, so the `set` below fails and
///   the **new, live** `AndroidApp` is dropped on the floor while the dead one is
///   kept. `bevy_winit` builds its event loop on that dead handle, polls a glue
///   instance belonging to an activity that no longer exists, and is handed no
///   window and no events. Nothing errors. The app is simply black, for good.
/// - a second Bevy `App` would be standing beside whatever the first one left —
///   two wgpu instances, two thread pools, two of every `OnceLock` this tree
///   initialises at startup.
///
/// The [`crate`-level] fix is that **the process ends when the activity does**,
/// which is what every other platform already does and what the code assumed all
/// along. `exit` rather than a return, so the glue's `ANativeActivity_finish`
/// never runs and there is no window left for a second `android_main` to be
/// created against; status 0 because leaving is the decision, not a fault —
/// `lib.rs::exit_on_halt` picks the same status for the same reason.
///
/// This is why the background connection (`systems::background`) survives the app
/// being *stopped* and not the activity being *destroyed*: while the activity is
/// merely off screen it is retained, the process is retained with it and the
/// socket in `network.rs`'s own thread never notices. Once Android has taken the
/// activity away there is nothing for the held connection to come back to, and
/// pretending otherwise is what produced the black screen. `ENTERED` is the
/// belt-and-braces for a second entry that arrives anyway (a framework version
/// that finishes differently, a path not thought of): it leaves rather than
/// starting an app that cannot draw, so the *next* launch gets a clean process.
///
/// `run` returns only on an `AppExit`: winit ignores `MainEvent::Destroy`, so a
/// destroyed activity never ends it. `java/.../MainActivity.java` covers that
/// case by killing the process in `onDestroy`; without it this guard is what a
/// relaunch trips over.
///
/// [`crate`-level]: crate
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: bevy::android::android_activity::AndroidApp) {
    use std::sync::atomic::Ordering;

    if ENTERED.swap(true, Ordering::SeqCst) {
        // Deliberately louder than a warning: this is the state that used to be
        // an unexplained black screen, and on a phone the log is the only
        // instrument there is. Through `bevy::log` rather than a `tracing`
        // dependency of its own — the subscriber that carries it to `logcat` is
        // the first App's `LogPlugin`, which is still installed, and this crate
        // is glue that should not grow a dependency list.
        bevy::log::error!(
            "android_main re-entered in a live process — the window handle from the \
             first activity cannot be replaced, so leaving instead of starting an \
             app that could never draw"
        );
        std::process::exit(0);
    }
    // `set` cannot fail here: the swap above is what guarantees the once-per-
    // process claim the old comment only asserted.
    let _ = bevy::android::ANDROID_APP.set(app);
    cartopolis::run();
    // `run` returned, so the activity is finished or finishing. See the note
    // above for why the process goes with it rather than waiting to be reused.
    bevy::log::info!("the activity is gone — ending the process with it");
    std::process::exit(0)
}

//! Android platform services: the three things the phone build could not do at
//! all, reached through JNI calls into the **framework** and nothing else.
//!
//! Everything here obeys one constraint, and it is not a stylistic one:
//! `docs/notes/android-build.md` — `android:hasCode="false"`, no `classes.dex`, no
//! Gradle, no Java or Kotlin source anywhere in the tree. So a design that needs a
//! class *we* define is not a design that can ship; it is a Gradle project wearing
//! a disguise. Two of the three items below were shaped by that and one was
//! rejected outright because of it:
//!
//! - **Location** (`start_location`) cannot use `LocationListener`, because that
//!   is an interface and implementing it needs a class. See the long comment on
//!   [`run_location`] for what it does instead and why.
//! - **Keep-screen-on** ([`set_keep_screen_on`]) is a `PowerManager.WakeLock`, and
//!   the reason it is not the window flag every Android app uses has nothing to do
//!   with the no-class rule: the NDK does carry the UI-thread hop we would
//!   otherwise have needed a `Runnable` for, and taking it deadlocked the app
//!   twice on android-activity's `RwLock`. See the function.
//! - **`open_url`** is plain framework classes (`Uri`, `Intent`, `Context`) and was
//!   never in doubt.
//!
//! **None of this has run on a device.** The container has no Android runtime
//! (`docs/notes/android-build.md`, "Verification status"), so every claim below is
//! read off the Android API contracts and the AOSP implementation rather than
//! measured. The comments mark which claims are *guaranteed by the documented
//! contract* and which are *inference that a device has to confirm* — that
//! distinction is the whole value of this file to whoever runs it first, and
//! flattening it would be worse than leaving the code unwritten.
//!
//! ## Threads, and why the JNI calls are where they are
//!
//! `android_main`'s thread is **not** the Java UI thread. android-activity creates
//! it, attaches it to the JVM and runs Bevy on it; the framework's own UI thread is
//! elsewhere, pumping the activity's looper. That matters three times over:
//!
//! 1. `FindClass` on a thread attached from native resolves against the **system**
//!    class loader, because there is no Java frame on the stack to take a loader
//!    from. Framework classes (`android/net/Uri`, `android/location/Location`, …)
//!    are on the boot classpath and resolve fine; an app class would not — which is
//!    academic here, since we have none.
//! 2. Local references created on a natively-attached thread are **not** freed by
//!    returning from a function, only by detaching or by popping a frame. Android's
//!    local-reference table aborts the process at 512 entries, so every JNI block in
//!    this file runs inside a [`JNIEnv::with_local_frame`]. A polling loop that
//!    leaks one reference per second is a crash forty-five minutes into a drive.
//! 3. Two calls here (`Activity.requestPermissions`, `Context.startActivity`) are
//!    made from a non-UI thread. Each carries its own note on what that costs.
//!
//! ## Errors
//!
//! `jni`'s checked calls return `Error::JavaException` when the JVM has a throw
//! pending, and deliberately leave it **pending** — after which every further JNI
//! call on that thread is undefined. So a failure is never simply propagated out of
//! this module: [`clear_pending`] describes the throw (which is what puts the Java
//! stack trace in `logcat`, the only instrument that exists on a phone) and clears
//! it before the `Err` crosses the boundary.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

use bevy::android::ANDROID_APP;
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};

use super::location::{GpsFix, Slot};

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

/// A JNI failure already reduced to a message.
///
/// It exists only so `?` works on `jni::errors::Result` inside a
/// [`JNIEnv::with_local_frame`] closure, whose error type must implement
/// `From<jni::errors::Error>` — `String` cannot, being neither ours nor jni's.
struct Fail(String);

impl From<jni::errors::Error> for Fail {
    fn from(e: jni::errors::Error) -> Self {
        Self(e.to_string())
    }
}

/// Describe and clear any pending Java exception.
///
/// Must be called on every path where a `jni` call returned `Err` — see the module
/// note. `exception_describe` is the only thing that puts the Java-side stack trace
/// in `logcat`; without it a failure here is a one-line Rust message about a method
/// that "threw", with no indication of what.
fn clear_pending(env: &JNIEnv) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
}

/// The process's `JavaVM`.
fn vm() -> Result<JavaVM, String> {
    let app = ANDROID_APP
        .get()
        .ok_or_else(|| "ANDROID_APP is unset — not running under android_main".to_string())?;
    // SAFETY: `vm_as_ptr` hands back the one `JavaVM` the framework gave
    // android-activity at startup. It is valid for the life of the process, which
    // outlives every use here.
    unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }
        .map_err(|e| format!("JavaVM::from_raw: {e}"))
}

/// The activity object, as a bare [`JObject`].
///
/// **This is a JNI *global* reference owned by `android-activity`, and we must
/// never free it.** Handing it back as a plain `JObject` — which has no `Drop` —
/// is what makes that structural: wrapping it in `AutoLocal` would delete it as
/// though it were local, and `GlobalRef` would delete a global we do not own.
/// Either leaves the next JNI call reading a dangling `jobject`, which is a crash
/// somewhere else entirely. Returns null off Android; callers reach this only
/// after [`vm`] has already established that `ANDROID_APP` is set.
fn activity() -> JObject<'static> {
    let Some(app) = ANDROID_APP.get() else {
        return JObject::null();
    };
    // SAFETY: a global reference the framework holds for the activity's lifetime;
    // borrowed, never freed (see above).
    unsafe { JObject::from_raw(app.activity_as_ptr().cast()) }
}

// ---------------------------------------------------------------------------
// The environment Android does not have
// ---------------------------------------------------------------------------

/// The system-property stand-in for an environment variable.
///
/// **This is the one seam in this file that is not JNI**, and that is why it
/// works: `__system_property_get` is bare libc, so it needs no framework class,
/// no `Runnable` and therefore no `classes.dex`. It is the whole trick.
///
/// The problem it solves is that every debugging switch in this tree —
/// `CARTO_NO_ATMOSPHERE`, `CARTO_FPS_LOG`, `CARTO_QUALITY`, `CARTO_NO_HAZE`,
/// `CARTO_NO_ENVMAP`, `CARTO_NO_OCEAN` — reaches the process through
/// [`crate::env_flag`] / [`crate::env_value`], i.e. `std::env::var_os`. A
/// NativeActivity is started by the zygote and inherits `app_process`'s
/// environment, which contains none of these and which nothing on the device can
/// add to: there is no shell to export from. So on the single platform whose
/// renderer most needs bisecting, every switch built for bisecting a renderer was
/// silently dead. `quality.rs` already documents this for one field and solves it
/// by moving the value into `Budget`; that does not generalise to a flag whose
/// entire purpose is to be flipped between two runs.
///
/// The mapping is mechanical: `CARTO_NO_ATMOSPHERE` → `debug.carto.no_atmosphere`.
/// The `debug.` prefix is load-bearing rather than decorative — it is the one
/// property namespace `adb shell setprop` may write **on a production build**
/// without root, which is what makes this reachable on the user's own phone
/// rather than only on an eng build:
///
/// ```text
/// adb shell setprop debug.carto.no_atmosphere 1
/// adb shell am force-stop eu.garage44.cartopolis   # properties are read at startup
/// ```
///
/// Two limits inherited from the platform, neither of which binds us today.
/// A value is capped at `PROP_VALUE_MAX` = 92 bytes including the NUL, so the
/// buffer below is exactly that and a longer value is truncated by the platform
/// rather than by us. And a *name* was capped at 31 bytes before Android 8; we
/// target 28+, where it is not, but the longest name this produces
/// (`debug.carto.no_atmosphere`, 25) fits the old limit anyway.
///
/// Returns `None` for an unset property, matching `env::var`'s shape. An empty
/// value is `Some("")` here and is filtered by the callers, exactly as the
/// environment path filters an empty variable.
pub fn system_property(name: &str) -> Option<String> {
    // `PROP_VALUE_MAX`, from `<sys/system_properties.h>`. Not re-exported by the
    // `libc` crate, so it is written out with its source rather than guessed.
    const PROP_VALUE_MAX: usize = 92;

    // The name mapping lives in `lib.rs`, not here, for one reason: it is the half
    // that can be wrong *silently* — a name that maps to a property nobody sets
    // reads as "off" forever — and a test beside it here would never run. This
    // module is `cfg(target_os = "android")`, no CI lane builds Android tests, and
    // `cargo check` does not compile `cfg(test)` at all (CLAUDE.md). So the pure
    // half sits on the native lane where `cargo test -p cartopolis` reaches it.
    let key = crate::android_property_name(name);
    let key = std::ffi::CString::new(key).ok()?;
    let mut buf = [0u8; PROP_VALUE_MAX];
    // SAFETY: `key` is a NUL-terminated C string that outlives the call, and `buf`
    // is `PROP_VALUE_MAX` bytes — the maximum the platform will write, including
    // its NUL. The return is the value's length, or 0 when the property is unset.
    let len = unsafe {
        libc::__system_property_get(key.as_ptr(), buf.as_mut_ptr().cast::<libc::c_char>())
    };
    if len <= 0 {
        return None;
    }
    // Trust the length rather than scanning for the NUL, but clamp it: a negative
    // value is already handled above, and a length past the buffer would be a
    // platform bug we should not turn into a panic.
    let len = (len as usize).min(PROP_VALUE_MAX);
    Some(String::from_utf8_lossy(&buf[..len]).into_owned())
}

// ---------------------------------------------------------------------------
// ADPF — telling the scheduler what we are actually trying to do
// ---------------------------------------------------------------------------

/// Android Dynamic Performance Framework: report each frame's real duration
/// against the one we are aiming for, so the governor clocks for the work rather
/// than guessing from thread busy-ness.
///
/// **Why this is worth reaching for.** Without it the kernel's EAS governor sees a
/// render thread that blocks in FIFO acquire (so it looks idle) and tile workers
/// that saturate a core for 120 ms at a stretch (so they look essential). That is
/// exactly backwards, and `nice` — which `platform::workers` also applies — only
/// adjusts the *relative* weight of two threads. It cannot tell the SoC that the
/// whole process has a 16.7 ms deadline it is missing, which is the thing that
/// actually raises clocks on a thermally-constrained 2-big/6-little part.
///
/// **Why it is reachable at all.** `APerformanceHint_*` are plain C functions in
/// `libandroid.so`. No Java interface, no callback, therefore no `Runnable` and no
/// `classes.dex` — the constraint that rules out `Window.setSustainedPerformanceMode`
/// (see this module's header) does not apply. This is the dex-free substitute for
/// it, and the better tool anyway.
///
/// **Why `dlsym` rather than linking.** The API is level 33; `build-apk.sh` builds
/// against the **platform-28** sysroot, whose `libandroid.so` stub does not export
/// these symbols, so a direct `extern "C"` reference fails at *link* time — on
/// every build, including devices that would have supported it. `ndk-sys` declares
/// them (`ffi_aarch64.rs`) but declaring is not linking. Resolving at run time
/// keeps `minSdkVersion 28` intact and degrades to nothing on an older handset.
///
/// A Fairphone 4 on Android 13 is API 33 exactly, i.e. the first version that has
/// this.
///
/// **Unverified on hardware**, like everything else in this file. The failure mode
/// is deliberately silent-and-inert (a null manager, a missing symbol and an
/// unsupported device all end at "do nothing"), so [`start_perf_hint`] logs which
/// way it went — without that line there is no way to tell "ADPF is helping" from
/// "ADPF was never running".
mod perf_hint {
    use std::ffi::c_void;
    use std::sync::Mutex;

    /// The live session, as an address rather than a pointer, so the static needs
    /// no `unsafe impl Send`. Zero means "no session".
    ///
    /// Behind a `Mutex` because `APerformanceHintSession` is not safe for
    /// concurrent use and the reporting system is not pinned to a thread — Bevy's
    /// executor may run it anywhere. The lock is uncontended in practice (one
    /// caller, once a frame) and the alternative, a `NonSend` resource, would force
    /// the system onto the main thread for no benefit.
    static SESSION: Mutex<usize> = Mutex::new(0);

    type GetManager = unsafe extern "C" fn() -> *mut c_void;
    type CreateSession = unsafe extern "C" fn(*mut c_void, *const i32, usize, i64) -> *mut c_void;
    type ReportActual = unsafe extern "C" fn(*mut c_void, i64) -> i32;
    type UpdateTarget = unsafe extern "C" fn(*mut c_void, i64) -> i32;

    struct Api {
        report: ReportActual,
        update_target: UpdateTarget,
    }

    static API: Mutex<Option<&'static Api>> = Mutex::new(None);

    /// `dlsym` one symbol out of `libandroid.so`.
    ///
    /// Via an explicit `dlopen` handle rather than `RTLD_DEFAULT`, because
    /// `RTLD_DEFAULT` is **not portable across our own ABIs**: bionic defines it as
    /// `(void*)0` on LP64 and `(void*)-1` on 32-bit, and `build-apk.sh` accepts
    /// `armeabi-v7a` as well as `arm64-v8a`. A null handle would silently resolve
    /// nothing on the 32-bit build — the one failure this whole module is shaped to
    /// avoid, since "no ADPF" and "ADPF misprobed" look identical from outside.
    ///
    /// `libandroid.so` is already loaded (the process links it for `ANativeWindow`
    /// and `AAssetManager`), so this bumps a refcount and hands back the existing
    /// handle. It is deliberately never `dlclose`d: the function pointers outlive
    /// the call and the library is wanted for the life of the process anyway.
    unsafe fn symbol(handle: *mut c_void, name: &[u8]) -> Option<*mut c_void> {
        let ptr = libc::dlsym(handle, name.as_ptr().cast());
        (!ptr.is_null()).then_some(ptr)
    }

    /// Open a session for `tid`, aiming at `target_nanos` per frame.
    ///
    /// Returns whether ADPF is now running, which the caller logs — this is the
    /// only externally visible difference between "supported and active" and
    /// "silently absent".
    pub fn start(tid: i32, target_nanos: i64) -> bool {
        let mut session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if *session != 0 {
            return true;
        }
        // SAFETY: each `dlsym` is checked for null before being transmuted, and the
        // signatures are transcribed from `ndk-sys`' own declarations for these
        // exact symbols. A wrong signature here would be UB, so they are not
        // guessed — see `ffi_aarch64.rs`.
        unsafe {
            let handle = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
            if handle.is_null() {
                return false;
            }
            // All four or none: a device that has the manager but is missing one of
            // the calls would leave a session nothing could report to.
            let (Some(get_manager), Some(create), Some(report), Some(update_target)) = (
                symbol(handle, b"APerformanceHint_getManager\0"),
                symbol(handle, b"APerformanceHint_createSession\0"),
                symbol(handle, b"APerformanceHint_reportActualWorkDuration\0"),
                symbol(handle, b"APerformanceHint_updateTargetWorkDuration\0"),
            ) else {
                return false;
            };
            let get_manager: GetManager = std::mem::transmute(get_manager);
            let create: CreateSession = std::mem::transmute(create);
            let report: ReportActual = std::mem::transmute(report);
            let update_target: UpdateTarget = std::mem::transmute(update_target);

            let manager = get_manager();
            if manager.is_null() {
                return false;
            }
            let threads = [tid];
            let handle = create(manager, threads.as_ptr(), threads.len(), target_nanos);
            if handle.is_null() {
                return false;
            }
            *session = handle as usize;
            *API.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::leak(Box::new(Api {
                report,
                update_target,
            })));
        }
        true
    }

    /// Report the frame that just finished. No-op when there is no session.
    pub fn report(actual_nanos: i64) {
        // The guard is held **across** the FFI call, not just long enough to copy
        // the handle out. `APerformanceHintSession` is not safe for concurrent use
        // and Bevy's executor may run the reporting system on any thread, so
        // releasing the lock before calling would leave exactly the race the mutex
        // is here to prevent. (It was written that way first.)
        let session = SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if *session == 0 {
            return;
        }
        let api = *API.lock().unwrap_or_else(|e| e.into_inner());
        let Some(api) = api else { return };
        // SAFETY: `*session` is a live handle from `createSession`, never closed —
        // the process owns it for its lifetime — and the guard above serialises the
        // calls the session is not itself safe for. `max(1)` because ADPF rejects a
        // non-positive duration, and a zero-length frame is what a paused clock
        // reports.
        unsafe {
            (api.report)(*session as *mut c_void, actual_nanos.max(1));
        }
    }

    /// Change the deadline we are aiming at — a display refresh change, or the
    /// frame cap moving.
    #[allow(dead_code)]
    pub fn set_target(target_nanos: i64) {
        let session = *SESSION.lock().unwrap_or_else(|e| e.into_inner());
        if session == 0 {
            return;
        }
        let api = *API.lock().unwrap_or_else(|e| e.into_inner());
        let Some(api) = api else { return };
        // SAFETY: as `report`.
        unsafe {
            (api.update_target)(session as *mut c_void, target_nanos.max(1));
        }
    }
}

/// Open the ADPF session for the calling thread. See [`perf_hint`].
pub fn start_perf_hint(target_nanos: i64) {
    // SAFETY: `gettid` takes no arguments and cannot fail.
    let tid = unsafe { libc::gettid() };
    if perf_hint::start(tid, target_nanos) {
        tracing::info!(
            tid,
            target_ms = target_nanos as f64 / 1.0e6,
            "ADPF performance hint session open"
        );
    } else {
        // Not a warning: an API-32 handset is not misconfigured, it simply predates
        // this. But it must be visible, or there is no way to tell an inactive
        // session from an ineffective one.
        tracing::info!("no ADPF on this device — frame hints are not available");
    }
}

/// Report the duration of the frame that just finished.
pub fn report_frame(actual_nanos: i64) {
    perf_hint::report(actual_nanos);
}

/// Move the ADPF deadline. See [`frame_rate`] for where the number comes from.
pub fn set_perf_hint_target(target_nanos: i64) {
    perf_hint::set_target(target_nanos);
}

/// Tell the compositor what frame rate this window wants, or that it has no
/// preference.
///
/// **`0.0` means "you choose", and it is what the client passes.** The NDK defines
/// it as *"the app will accept the system's choice of display frame rate"*, and
/// the system is the only party in this conversation that knows the panel's modes,
/// its thermal state and what else is on screen. This used to pass a hard 60 —
/// which is a real answer to a real problem (a scene budgeted for 30 renders four
/// times over on a 120 Hz panel and is throttled for it) built out of a number
/// nobody could check. On the 120 Hz handset this client targets it pinned the
/// display to half its rate. See `lib.rs::report_frame_to_scheduler`.
///
/// A non-zero rate is still honoured, for a caller that has measured one.
///
/// `ANativeWindow_setFrameRate` is a *hint* either way: the platform may choose a
/// multiple of what is asked for, and on a device with no seamless refresh-rate
/// switching it may do nothing at all. That is why this reports what it did.
///
/// API 30+, `dlsym`ed for exactly the reason [`perf_hint`] gives — an API-29
/// handset is not misconfigured, and a missing symbol must be distinguishable
/// from a misprobed one.
pub fn set_frame_rate(fps: f32) -> bool {
    use std::ffi::c_void;
    /// `ANativeWindow_setFrameRate(window, fps, compatibility)`.
    type SetFrameRate = unsafe extern "C" fn(*mut c_void, f32, i8) -> i32;
    /// `ANATIVEWINDOW_FRAME_RATE_COMPATIBILITY_DEFAULT` — "this is the rate I
    /// want", as opposed to `FIXED_SOURCE`, which is for video whose cadence is
    /// fixed by its content and must not be resampled.
    const COMPATIBILITY_DEFAULT: i8 = 0;

    let Some(app) = ANDROID_APP.get() else {
        return false;
    };
    let Some(window) = app.native_window() else {
        return false; // no surface yet; the caller retries once there is one
    };
    // SAFETY: `dlsym` is null-checked before the transmute, and the signature is
    // transcribed from `ndk-sys`' declaration of this symbol rather than guessed.
    // The window pointer is borrowed for the duration of the call only — the
    // `NativeWindow` guard above keeps it alive across it.
    unsafe {
        let handle = libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW);
        if handle.is_null() {
            return false;
        }
        let ptr = libc::dlsym(handle, c"ANativeWindow_setFrameRate".as_ptr());
        if ptr.is_null() {
            tracing::info!("no ANativeWindow_setFrameRate on this device (API < 30)");
            return false;
        }
        let set: SetFrameRate = std::mem::transmute(ptr);
        let rc = set(window.ptr().as_ptr().cast(), fps, COMPATIBILITY_DEFAULT);
        if rc == 0 {
            // `debug`, because this is no longer a one-shot at startup: the idle
            // throttle re-asks every time you stop and start panning, and an hour
            // of that at `info` is not a log anybody can read. The caller logs the
            // first successful ask, which is the line a session wants.
            if fps == 0.0 {
                tracing::debug!("left the frame rate to the compositor — the panel decides");
            } else {
                tracing::debug!(fps, "asked the compositor for a frame rate");
            }
            true
        } else {
            tracing::info!(fps, rc, "the compositor refused the frame rate hint");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Keep the screen on
// ---------------------------------------------------------------------------

/// `PowerManager.SCREEN_BRIGHT_WAKE_LOCK`. Deprecated since API 17 in favour of
/// `FLAG_KEEP_SCREEN_ON` — and taken anyway, because the flag is exactly what we
/// cannot reach from here (see [`set_keep_screen_on`]). Deprecated is not removed:
/// `PowerManagerService` still honours the level, and if a future release stops
/// doing so the cost is a screen that times out mid-drive, not a broken app.
const SCREEN_BRIGHT_WAKE_LOCK: i32 = 0x0000_000a;

/// Android wants `app:reason`, and logs a complaint about a tag that isn't shaped
/// that way. It is also what shows up in `dumpsys power`, which is how anyone
/// chasing a battery drain finds out it was us.
const WAKE_LOCK_TAG: &str = "cartopolis:driving";

/// The one thread that touches the wake lock. See [`set_keep_screen_on`].
///
/// `None` if the thread could not be spawned; the screen is then simply never held
/// awake, which costs a screen timeout rather than a frozen app.
static WAKE_LOCK: OnceLock<Option<mpsc::Sender<bool>>> = OnceLock::new();

/// Hold the screen awake while driving (driver mode's reason for existing: the
/// phone otherwise blanks thirty seconds into a drive).
///
/// ## Why this is a wake lock and not the window flag
///
/// The Android-shaped answer is `Window.addFlags(FLAG_KEEP_SCREEN_ON)`. It has to
/// run on the Java UI thread, and `Activity.runOnUiThread` takes a `Runnable`, i.e.
/// a class we cannot define. The NDK carries the hop for us —
/// `ANativeActivity_setWindowFlags` writes a `CMD_SET_WINDOW_FLAGS` work item into
/// the activity's main-work pipe and the UI thread's looper performs the
/// `setFlags`, and *that* call is thread-safe by construction. So this was written
/// against `AndroidApp::set_window_flags`, twice, and froze the app both times.
///
/// **The obstacle is a Rust lock, not Android.** `AndroidApp::set_window_flags` is
/// the only method in this process that takes `AndroidApp::inner.write()` — winit
/// and Bevy take nothing but `read()`. And `AndroidApp::poll_events` holds
/// `inner.read()` *across its callback*, which on this stack is winit's
/// `single_iteration`, which is the entire Bevy update — inside which winit calls
/// `input_events_iter()` and `native_window()`, both of which take `inner.read()`
/// **again, on the same thread**. `std::sync::RwLock` parks a reader behind a
/// waiting writer to keep writers from starving, so:
///
/// ```text
/// android_main ─read()─▶ poll_events ─▶ winit ─▶ Bevy update ─┐
///        ▲                                                    │ native_window()
///        │                                        ─read()─ blocked, writer waiting
///        └── outer guard never drops                          ▼
///            wake-lock thread ─write()─▶ set_window_flags ─ blocked on the guard
/// ```
///
/// Three parties, one cycle, and it needs no bad luck beyond the write landing
/// mid-frame. The first fix (2026-08-14) moved the call off the frame's own thread,
/// which turned a certain self-deadlock into a race — entering driver mode won it,
/// leaving driver mode lost it, and the report on 2026-08-15 is the ANR: `asking`
/// with no `done` in the log, `carto winflags` parked on a futex inside our own
/// `.so` with no `libandroid.so` frame above it, and every Bevy thread waiting on a
/// frame that will not end. There is no third position to move the call to: every
/// point we can reach from Bevy is inside that read guard, and android-activity 0.6
/// exposes neither a `try_` variant nor the raw `ANativeActivity*` that would let
/// us skip the wrapper (`activity_as_ptr` hands back the `jobject`, not the struct).
///
/// `PowerManager.WakeLock` sidesteps all of it: `acquire`/`release` are ordinary
/// binder calls, safe from any thread, needing no UI-thread hop and therefore no
/// `Runnable` — and, the point here, never touching `AndroidApp::inner`. With this
/// change nothing in the process takes that write lock, so the re-entrant reads
/// winit performs every frame can no longer be made to block by anything we do.
///
/// It costs the `WAKE_LOCK` permission (normal, granted at install) and a
/// deprecated level constant.
///
/// *Needs a device:* that `SCREEN_BRIGHT_WAKE_LOCK` still keeps the display lit on
/// current Android. The `held` field on the `keep-screen-on: done` line is
/// `WakeLock.isHeld()` read back after the call, which proves the lock was taken —
/// not that the display obeyed it. Watch the screen; that is the measurement.
pub fn set_keep_screen_on(on: bool) {
    if ANDROID_APP.get().is_none() {
        return;
    }
    // One thread, one queue: values are applied in the order they were asked for,
    // so a rapid off/on cannot land as on/off. It also keeps the `WakeLock` global
    // reference on a single thread, and keeps the binder round-trip off the frame.
    let tx = WAKE_LOCK.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<bool>();
        let spawned = std::thread::Builder::new()
            // 15 characters — Android truncates a thread name there, and this one
            // has to stay legible in the watchdog's stalled-process dump.
            .name("carto wakelock".to_string())
            .spawn(move || {
                // The lock is made once, on first use, and then reused: creating a
                // fresh one per acquire would leave the old one to a finalizer, and
                // a screen lock released by the GC is released whenever it likes.
                let mut lock: Option<GlobalRef> = None;
                for want in rx {
                    apply_keep_screen_on(&mut lock, want);
                }
            });
        match spawned {
            // Detached: the queue is the whole protocol, and the thread ends when
            // the sender in this `OnceLock` is dropped, i.e. at process exit.
            Ok(_handle) => Some(tx),
            Err(e) => {
                tracing::warn!(error = %e, "could not start the wake-lock thread");
                None
            }
        }
    });
    if let Some(tx) = tx {
        let _ = tx.send(on);
    }
}

/// Perform the acquire or release. **Only ever called on the `carto wakelock`
/// thread**, which owns `lock`.
///
/// The bracketed `asking`/`done` pair is a diagnostic, not noise: on a platform
/// where a blocked call is indistinguishable from a system that never ran, this is
/// what separates the two in one line of the next report. It has now caught this
/// bug twice and it stays to catch the next one.
fn apply_keep_screen_on(lock: &mut Option<GlobalRef>, on: bool) {
    tracing::info!(on, "keep-screen-on: asking");
    match set_wake_lock(lock, on) {
        Ok(held) => tracing::info!(on, held, "keep-screen-on: done"),
        Err(msg) => tracing::warn!(on, error = %msg, "keep-screen-on: failed"),
    }
}

/// Acquire or release the lock, making it first if there isn't one, and report
/// `WakeLock.isHeld()` afterwards.
fn set_wake_lock(lock: &mut Option<GlobalRef>, on: bool) -> Result<bool, String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;
    let activity = activity();
    if lock.is_none() {
        match new_wake_lock(&mut env, &activity) {
            Ok(made) => *lock = Some(made),
            Err(Fail(msg)) => {
                clear_pending(&env);
                return Err(msg);
            }
        }
    }
    let Some(lock) = lock.as_ref() else {
        return Err("no wake lock".to_string());
    };
    match hold_wake_lock(&mut env, lock, on) {
        Ok(held) => Ok(held),
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

/// `PowerManager.newWakeLock(SCREEN_BRIGHT_WAKE_LOCK, tag)`, reference counting
/// off.
///
/// Uncounted on purpose. The caller is edge-triggered (`lib.rs`'s
/// `apply_screen_wakelock` only sends when the answer changes), so a counted lock
/// would balance — but *would* is doing the work in that sentence, and the failure
/// mode of a miscount is a screen held awake until the process dies, which the user
/// experiences as a flat battery and cannot attribute to anything. Uncounted,
/// `release` on a lock that isn't held is a no-op rather than the `RuntimeException`
/// a counted one throws.
fn new_wake_lock(env: &mut JNIEnv, activity: &JObject) -> Result<GlobalRef, Fail> {
    env.with_local_frame::<_, _, Fail>(8, |env| {
        // `Context.POWER_SERVICE`. A static-field read would need the field's class
        // and its type; the value has been "power" since API 1 and is part of the
        // public contract (same reasoning as `location_manager`).
        let name: JObject = env.new_string("power")?.into();
        let power = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        if power.as_raw().is_null() {
            return Err(Fail(
                "getSystemService(\"power\") returned null".to_string(),
            ));
        }
        let tag: JObject = env.new_string(WAKE_LOCK_TAG)?.into();
        let lock = env
            .call_method(
                &power,
                "newWakeLock",
                "(ILjava/lang/String;)Landroid/os/PowerManager$WakeLock;",
                &[JValue::Int(SCREEN_BRIGHT_WAKE_LOCK), JValue::Object(&tag)],
            )?
            .l()?;
        if lock.as_raw().is_null() {
            return Err(Fail("newWakeLock returned null".to_string()));
        }
        env.call_method(
            &lock,
            "setReferenceCounted",
            "(Z)V",
            &[JValue::Bool(u8::from(false))],
        )?;
        Ok(env.new_global_ref(&lock)?)
    })
}

/// `WakeLock.acquire()` / `WakeLock.release()`, then read the state back.
fn hold_wake_lock(env: &mut JNIEnv, lock: &GlobalRef, on: bool) -> Result<bool, Fail> {
    env.with_local_frame::<_, _, Fail>(4, |env| {
        if on {
            // The untimed overload deliberately: a timeout would release the lock
            // mid-drive, and the release here is driven by leaving driver mode.
            env.call_method(lock.as_obj(), "acquire", "()V", &[])?;
        } else {
            env.call_method(lock.as_obj(), "release", "()V", &[])?;
        }
        Ok(env.call_method(lock.as_obj(), "isHeld", "()Z", &[])?.z()?)
    })
}

// ---------------------------------------------------------------------------
// 4. Soft input — a probe, not a feature
// ---------------------------------------------------------------------------

/// Ask the IME to appear or go away. **This is a probe and should be treated as
/// one.**
///
/// `docs/notes/android-build.md` records the NativeActivity trade: there is no
/// `InputConnection`, so an IME that commits text through `commitText` — which is
/// most of them, and all of the gesture-typing ones — produces *nothing*, while one
/// that falls back to dispatching `KeyEvent`s delivers characters. The keyboard
/// appearing therefore proves nothing about whether typing works.
///
/// We are not designing around this: the real answer to text entry on the phone is
/// an in-app on-screen keyboard, built separately. This exists so that **one** run
/// on **one** device can settle which side of that split the stock IME falls on,
/// and so the answer is a measurement rather than an argument. Wire it to a debug
/// affordance, try it, write the result into the note, and expect this function to
/// be deleted rather than promoted.
///
/// `show_implicit`/`hide_implicit_only` are both passed as *explicit*: an implicit
/// show lets the IME decline (e.g. when a hardware keyboard is attached), which
/// would make a negative result unreadable — "nothing happened" has to mean the
/// framework refused, not that we asked politely.
// `allow`: `platform` is a *private* module, so a `pub fn` nothing in the crate
// calls is dead code. This one is expected to stay uncalled in shipped code —
// wire it to a debug affordance for the one run, then delete it.
#[allow(dead_code)]
pub fn show_soft_input(show: bool) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    if show {
        app.show_soft_input(false);
    } else {
        app.hide_soft_input(false);
    }
}

// ---------------------------------------------------------------------------
// The two facts the compass needs from the framework
// ---------------------------------------------------------------------------
//
// `crate::platform::orientation`'s Android arm is *not* JNI — it is the NDK's C
// sensor API, precisely because `SensorEventListener` is an interface and this APK
// ships no class to implement it. But two of its inputs have no C equivalent at
// all, so they are here, where the JVM plumbing already lives.

/// The display's rotation in degrees (0/90/180/270), or `None` if it cannot be
/// read.
///
/// This is the `screen_angle_deg` of [`crate::platform::orientation::screen_up_bearing`]
/// — how far the *content* is turned clockwise inside the device to stay upright
/// — and `Display.getRotation()` is defined as exactly that quantity, in quarter
/// turns. Without it the compass is 90° out in landscape and 180° out upside
/// down, which is the orientation half of a map app gets held in.
///
/// `WindowManager.getDefaultDisplay()` is deprecated in favour of
/// `Context.getDisplay()` (API 30), and is used anyway: it is not removed, it
/// works from API 1 to 36, and the replacement would need a version branch to
/// avoid throwing on our API 28 floor. The deprecation is about multi-display
/// correctness on a *non-visual* context; an `Activity` has exactly one display
/// and it is the one the user is looking at.
///
/// *Needs a device:* that this reports the rotation of the display the activity is
/// on rather than of the physical panel, on a phone that has been turned.
pub fn display_rotation_deg() -> Option<i32> {
    let java_vm = vm().ok()?;
    let mut env = java_vm.attach_current_thread().ok()?;
    let activity = activity();
    match display_rotation_inner(&mut env, &activity) {
        Ok(deg) => Some(deg),
        Err(Fail(msg)) => {
            clear_pending(&env);
            tracing::warn!(error = %msg, "could not read the display rotation");
            None
        }
    }
}

fn display_rotation_inner(env: &mut JNIEnv, activity: &JObject) -> Result<i32, Fail> {
    env.with_local_frame(6, |env| {
        let wm = env
            .call_method(
                activity,
                "getWindowManager",
                "()Landroid/view/WindowManager;",
                &[],
            )?
            .l()?;
        let display = env
            .call_method(&wm, "getDefaultDisplay", "()Landroid/view/Display;", &[])?
            .l()?;
        let rotation = env.call_method(&display, "getRotation", "()I", &[])?.i()?;
        // `Surface.ROTATION_0..3` are quarter turns, not degrees.
        Ok::<i32, Fail>(rotation.rem_euclid(4) * 90)
    })
}

/// The URL this launch was started with, if it was started by tapping a link:
/// `getIntent().getDataString()`.
///
/// This is how a `cartopolis://` link and a verified `https://cartopolis.org/go`
/// App Link reach the client (`systems::place_link`). Every class here is a
/// framework one, so — like `open_url` — this worked back when the APK carried no
/// `classes.dex` at all.
///
/// **It answers for the launch, and only the launch.** `android-activity` 0.6
/// surfaces no intent at all, and `android.app.NativeActivity` does not override
/// `onNewIntent`, so nothing calls `setIntent` and `getIntent` keeps handing back
/// the intent that *started* the activity for as long as the process lives. With
/// `launchMode="singleTask"` in the manifest, tapping a second link while the app
/// is already running therefore brings the existing task forward showing the place
/// it was already at, and this function reports the *first* link again. The fix is
/// a trampoline activity, which is a Java class — and as of the dex lane
/// (`build-apk.sh`, `report_dex`) the APK *can* carry one, so this is now unbuilt
/// work rather than the platform limit it used to be
/// (`docs/notes/android-build.md`). Until it is built, callers must read this
/// **once**, at startup, or a link followed hours ago will teleport the camera
/// again.
///
/// Returns `None` for an ordinary launcher tap, which carries an `ACTION_MAIN`
/// intent with no data — the overwhelmingly common case, and not a failure.
pub fn launch_url() -> Option<String> {
    let java_vm = vm().ok()?;
    let mut env = java_vm.attach_current_thread().ok()?;
    let activity = activity();
    let result = env.with_local_frame(6, |env| {
        let intent = env
            .call_method(&activity, "getIntent", "()Landroid/content/Intent;", &[])?
            .l()?;
        if intent.is_null() {
            return Ok::<Option<String>, Fail>(None);
        }
        // `getDataString()` rather than `getData()` + `toString()`: one call, and it
        // is already null-for-absent, which is the answer we want for a launcher tap.
        let data = env
            .call_method(&intent, "getDataString", "()Ljava/lang/String;", &[])?
            .l()?;
        if data.is_null() {
            return Ok(None);
        }
        let url: String = env.get_string(&JString::from(data))?.into();
        Ok(Some(url))
    });
    match result {
        Ok(url) => url.filter(|u| !u.trim().is_empty()),
        Err(Fail(msg)) => {
            clear_pending(&env);
            tracing::warn!(error = %msg, "could not read the launch intent");
            None
        }
    }
}

/// The app's package name (`eu.garage44.cartopolis`), or `None`.
///
/// Read rather than hardcoded: the manifest owns it, and a constant here would be
/// a second place to change it that nothing would notice going stale. Its only use
/// is `ASensorManager_getInstanceForPackage`, which uses it to attribute the
/// sensor's power draw to this app in the battery breakdown.
pub fn package_name() -> Option<String> {
    let java_vm = vm().ok()?;
    let mut env = java_vm.attach_current_thread().ok()?;
    let activity = activity();
    let result = env.with_local_frame(4, |env| {
        let name = env
            .call_method(&activity, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        let s: String = env.get_string(&JString::from(name))?.into();
        Ok::<String, Fail>(s)
    });
    match result {
        Ok(name) => Some(name),
        Err(Fail(msg)) => {
            clear_pending(&env);
            tracing::warn!(error = %msg, "could not read the package name");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The APK's own Java
// ---------------------------------------------------------------------------

/// The probe class, in the two spellings JNI insists on: `FindClass` takes an
/// internal name with slashes, `ClassLoader.loadClass` takes a binary name with
/// dots. They are the same class, and mixing them up yields a
/// `ClassNotFoundException` that says nothing about which of the two was wrong.
const DEX_PROBE_INTERNAL: &str = "eu/garage44/cartopolis/DexProbe";
const DEX_PROBE_BINARY: &str = "eu.garage44.cartopolis.DexProbe";

/// Call the one method in the APK's `classes.dex` and log whether it worked.
///
/// This is the runtime half of the dex lane that `build-apk.sh` now compiles
/// (see `java/eu/garage44/cartopolis/DexProbe.java` for the build half and the
/// reasoning). It runs once at `Startup`, and it exists because the interesting
/// question is not whether a dex is *in* the APK — `unzip -l` answers that at
/// build time — but whether our own classes are reachable from the thread this
/// process actually runs Rust on. Those are different questions with different
/// answers, which is the whole reason this is instrumented rather than assumed:
///
/// **`JNIEnv::FindClass` resolves against the classloader of the calling frame,
/// and a natively-attached thread has no Java frame at all.** JNI then falls
/// back to the *system* classloader, which knows the framework and nothing about
/// this APK. `android_main` runs on a thread `android-activity` spawned — never
/// the Java main thread — so a bare `find_class` for one of our classes fails
/// here with `ClassNotFoundException` while the identical call would succeed
/// from a JNI method invoked by Java. Nothing about the failure hints that the
/// *thread* was the problem, and it is the standard way an Android JNI port
/// loses an afternoon.
///
/// The fix is to ask an object that *does* know: `Context.getClassLoader()` on
/// the activity returns the app's own loader, and `loadClass` on that finds our
/// class from any thread. Every future Java class — the foreground service
/// first — needs this same route, so the probe reports which of the two worked
/// rather than only that one of them did. If `via_find_class` is ever `true` on
/// a real device, this comment is wrong and the dance can go.
pub fn report_dex() {
    match dex_probe() {
        Ok((via_find_class, probe)) => tracing::info!(
            via_find_class,
            probe = %probe,
            "classes.dex is loaded and callable"
        ),
        // `error!`, not `warn!`: nothing else in the app has failed, but every
        // Java-shaped feature planned on top of this lane is dead if it does,
        // and on a phone this line is the only place that would ever say so.
        Err(msg) => tracing::error!(error = %msg, "classes.dex is NOT reachable"),
    }
}

/// `(the bare FindClass worked, what the class returned)`.
fn dex_probe() -> Result<(bool, String), String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;
    let activity = activity();
    match dex_probe_inner(&mut env, &activity) {
        Ok(answer) => Ok(answer),
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

fn dex_probe_inner(env: &mut JNIEnv, activity: &JObject) -> Result<(bool, String), Fail> {
    env.with_local_frame(8, |env| {
        // Tried first, and only so the log records the answer — see the note
        // above. The failure leaves a `ClassNotFoundException` pending, and a
        // pending throw makes every later JNI call on this thread undefined, so
        // it is cleared here rather than at the outer boundary.
        let via_find_class = env.find_class(DEX_PROBE_INTERNAL).is_ok();
        if !via_find_class {
            clear_pending(env);
        }

        let class = load_app_class(env, activity, DEX_PROBE_BINARY)?;
        let text = env
            .call_static_method(class, "probe", "()Ljava/lang/String;", &[])?
            .l()?;
        let text: String = env.get_string(&JString::from(text))?.into();
        Ok::<(bool, String), Fail>((via_find_class, text))
    })
}

/// Load one of *our* classes by binary name, from any thread.
///
/// The route that works, for the reason [`report_dex`] documents at length:
/// `Context.getClassLoader()` on the activity, then `loadClass`. Use this for
/// every class in `crates/cartopolis_android/java/` and `env.find_class` for
/// framework ones — `android.content.Intent` and friends are in the *system*
/// loader, which is exactly the one a natively-attached thread gets, so those
/// resolve directly and do not need the hop.
///
/// Must be called inside a local frame: the returned `JClass` is a local
/// reference belonging to whatever frame is current.
fn load_app_class<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject,
    binary_name: &str,
) -> Result<JClass<'local>, Fail> {
    let loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let name = env.new_string(binary_name)?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    Ok(JClass::from(class))
}

/// `eu.garage44.cartopolis.ConnectionService`, the foreground service.
const CONNECTION_SERVICE_BINARY: &str = "eu.garage44.cartopolis.ConnectionService";

/// Start or stop the foreground service that keeps this process alive while the
/// app is off screen (`ConnectionService.java`, `systems::background`).
///
/// **Start this while the app is on screen.** Since Android 12 a
/// `startForegroundService` from the background throws
/// `ForegroundServiceStartNotAllowedException`, and the app-visible symptom would
/// be a crash at the worst possible moment — as the user leaves. That is the
/// reason the caller keys this on the *setting* (flipped in Settings, with the
/// app plainly in front) rather than on `AppLifecycle::WillSuspend`, which is
/// already halfway out.
///
/// Stopping does **not** close the connection. The socket lives in `network.rs`'s
/// own thread and neither knows nor cares about this; what stops is the
/// permission to keep running once the activity is gone.
pub fn set_background_service(on: bool) -> Result<(), String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("attach_current_thread: {e}"))?;
    let activity = activity();
    match set_background_service_inner(&mut env, &activity, on) {
        Ok(()) => Ok(()),
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

fn set_background_service_inner(
    env: &mut JNIEnv,
    activity: &JObject,
    on: bool,
) -> Result<(), Fail> {
    env.with_local_frame(8, |env| {
        let service = load_app_class(env, activity, CONNECTION_SERVICE_BINARY)?;
        // `Intent(Context, Class<?>)` — the explicit-component constructor, which
        // is what `exported="false"` in the manifest requires: an implicit intent
        // could not reach a non-exported component at all.
        let intent = env.new_object(
            "android/content/Intent",
            "(Landroid/content/Context;Ljava/lang/Class;)V",
            &[JValue::Object(activity), JValue::Object(&service)],
        )?;
        if on {
            // Not `startService`: from API 26 that throws for a background start,
            // and this one contracts to call `startForeground` within five
            // seconds — which `onStartCommand` does as its first act.
            env.call_method(
                activity,
                "startForegroundService",
                "(Landroid/content/Intent;)Landroid/content/ComponentName;",
                &[JValue::Object(&intent)],
            )?;
        } else {
            env.call_method(
                activity,
                "stopService",
                "(Landroid/content/Intent;)Z",
                &[JValue::Object(&intent)],
            )?;
        }
        Ok::<(), Fail>(())
    })
}

// ---------------------------------------------------------------------------
// 3. open_url
// ---------------------------------------------------------------------------

/// `Intent.FLAG_ACTIVITY_NEW_TASK`. Required, not decorative — see below.
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

/// Open a URL in whatever the device treats as a browser.
///
/// Every class touched here is a framework class (`android.net.Uri`,
/// `android.content.Intent`, `android.content.Context`), so this is the one item of
/// the three that the no-dex rule never threatened.
///
/// Two choices are load-bearing:
///
/// **It starts the activity from the *application* context, not the `Activity`.**
/// `Activity.startActivity` ends up in `cancelInputsAndStartExitTransition`, which
/// reads the window's decor view — a UI-thread structure, and we are on Bevy's
/// thread. `ContextImpl.startActivity` is a plain binder call with no view access,
/// which makes the cross-thread call harmless rather than merely usually-harmless.
/// The price is that `FLAG_ACTIVITY_NEW_TASK` becomes mandatory: an app context has
/// no task to launch into, and `ContextImpl` throws `AndroidRuntimeException`
/// saying so if the flag is absent.
///
/// **Nothing resolves the intent first.** Since Android 11 package visibility makes
/// `resolveActivity` answer `null` for apps we have not declared a `<queries>` for,
/// while `startActivity` itself is not filtered and web browsers are visible to
/// every app by default. So a resolve check would report "no browser" on a phone
/// that has three; catching `ActivityNotFoundException` out of the start is the
/// honest test. *Needs a device:* that a phone with no browser at all reaches the
/// `Err` arm here rather than something stranger.
///
/// Callers own the scheme policy — `data_layers::open_url` already restricts to
/// http(s) because an OSM `website` tag is untrusted data, and this must not be
/// wired to anything that skips that step.
// `allow`: see `show_soft_input`. `data_layers::open_url`'s catch-all arm is the
// call site waiting for this. Note the attribute also keeps `open_url_inner` and
// `FLAG_ACTIVITY_NEW_TASK` live — rustc's dead-code pass seeds its worklist from
// `allow(dead_code)` items, so everything reachable from here is spared too.
#[allow(dead_code)]
pub fn open_url(url: &str) -> Result<(), String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;
    let activity = activity();
    match open_url_inner(&mut env, &activity, url) {
        Ok(()) => {
            tracing::info!(%url, "handed to the system browser");
            Ok(())
        }
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

fn open_url_inner(env: &mut JNIEnv, activity: &JObject, url: &str) -> Result<(), Fail> {
    env.with_local_frame(12, |env| {
        let url_str: JObject = env.new_string(url)?.into();
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&url_str)],
            )?
            .l()?;
        let action: JObject = env.new_string("android.intent.action.VIEW")?.into();
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        // Returns the Intent for chaining; the returned reference is a local one
        // this frame will free.
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)],
        )?;
        let app_ctx = env
            .call_method(
                activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )?
            .l()?;
        env.call_method(
            &app_ctx,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
}

/// Send the app to the background, as Back on a messenger's root screen does.
///
/// Back from the root used to write `AppExit`, which on this platform ends the
/// process — the activity cannot be recreated inside a live one
/// (`MainActivity.java`) — so the next tap on the icon started from nothing:
/// a reload of the map, a sign-in, the Chats tab gone, any call dropped.
/// Android itself has done the same as this since 12 for a launcher's root
/// activity, and Signal and WhatsApp do it everywhere.
///
/// `Activity.moveTaskToBack` is a binder call, safe from any thread and taking
/// none of `AndroidApp`'s locks (`set_keep_screen_on` has why that matters);
/// it runs on a thread of its own anyway, to keep the round-trip off the frame.
pub fn move_task_to_back() {
    if ANDROID_APP.get().is_none() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("carto to-back".to_string())
        .spawn(|| {
            let result = vm().and_then(|java_vm| {
                let mut env = java_vm
                    .attach_current_thread()
                    .map_err(|e| format!("attach_current_thread: {e}"))?;
                let activity = activity();
                let moved =
                    env.call_method(&activity, "moveTaskToBack", "(Z)Z", &[JValue::Bool(1)]);
                if let Err(e) = moved {
                    clear_pending(&env);
                    return Err(format!("moveTaskToBack: {e}"));
                }
                Ok(())
            });
            if let Err(e) = result {
                tracing::warn!(error = %e, "could not move the app to the background");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not move the app to the background");
    }
}

/// Hand a block of text to the system share sheet.
///
/// This exists so a crash on a phone can be read **without a workstation**. The
/// whole diagnostic story for Android otherwise assumes `adb`: logcat needs a
/// cable, an app may only read its own log since Android 4.1, and the app's own
/// files live under `getFilesDir()`, which nothing on a non-rooted device can
/// open. A share sheet sidesteps all of it — the user picks where the text goes,
/// so it is also not telemetry: nothing leaves the device unless they send it.
///
/// It is deliberately usable **before Bevy exists**. `ACTION_SEND` is a plain
/// binder call through the application context, needing no window, no surface and
/// no renderer, which matters because the failure being reported is one that
/// killed the app before it could draw anything.
pub fn share_text(subject: &str, body: &str) -> Result<(), String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;
    let activity = activity();
    match share_text_inner(&mut env, &activity, subject, body) {
        Ok(()) => Ok(()),
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

fn share_text_inner(
    env: &mut JNIEnv,
    activity: &JObject,
    subject: &str,
    body: &str,
) -> Result<(), Fail> {
    env.with_local_frame::<_, _, Fail>(16, |env| {
        let action: JObject = env.new_string("android.intent.action.SEND")?.into();
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action)],
        )?;
        let mime: JObject = env.new_string("text/plain")?.into();
        env.call_method(
            &intent,
            "setType",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&mime)],
        )?;
        let key_subject: JObject = env.new_string("android.intent.extra.SUBJECT")?.into();
        let val_subject: JObject = env.new_string(subject)?.into();
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&key_subject), JValue::Object(&val_subject)],
        )?;
        let key_text: JObject = env.new_string("android.intent.extra.TEXT")?.into();
        let val_text: JObject = env.new_string(body)?.into();
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&key_text), JValue::Object(&val_text)],
        )?;
        // A chooser, not the bare intent: without one Android may silently pick a
        // remembered default, and the point here is that the user sees the text
        // and decides where it goes.
        let title: JObject = env.new_string("Cartopolis crash report")?.into();
        let chooser = env
            .call_static_method(
                "android/content/Intent",
                "createChooser",
                "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;",
                &[JValue::Object(&intent), JValue::Object(&title)],
            )?
            .l()?;
        env.call_method(
            &chooser,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)],
        )?;
        // The *application* context, for the reason `open_url` gives: the
        // activity's own `startActivity` touches the decor view from this thread.
        let app_ctx = env
            .call_method(
                activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )?
            .l()?;
        env.call_method(
            &app_ctx,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&chooser)],
        )?;
        Ok(())
    })
}

/// Put `text` on the system clipboard (`ClipboardManager.setPrimaryClip`), for
/// `utils::clipboard::set_text` — so a developer log or a place link can be
/// copied on the phone and pasted into a chat.
///
/// Through the *application* context, like [`share_text`]: it is a binder call
/// that needs no window, so any thread attached to the JVM may make it. Android
/// 10+ only lets an app *read* the clipboard while focused; writing is allowed,
/// and Android 13+ shows its own "copied" toast.
pub fn copy_text(label: &str, text: &str) -> Result<(), String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;
    let activity = activity();
    let result = env.with_local_frame::<_, _, Fail>(8, |env| {
        let app_ctx = env
            .call_method(
                &activity,
                "getApplicationContext",
                "()Landroid/content/Context;",
                &[],
            )?
            .l()?;
        let service: JObject = env.new_string("clipboard")?.into();
        let manager = env
            .call_method(
                &app_ctx,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )?
            .l()?;
        if manager.is_null() {
            return Err(Fail("no clipboard service".into()));
        }
        let label: JObject = env.new_string(label)?.into();
        let text: JObject = env.new_string(text)?.into();
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&text)],
            )?
            .l()?;
        env.call_method(
            &manager,
            "setPrimaryClip",
            "(Landroid/content/ClipData;)V",
            &[JValue::Object(&clip)],
        )?;
        Ok(())
    });
    match result {
        Ok(()) => Ok(()),
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

/// The number of trailing log lines [`own_logcat`] asks logd for.
///
/// **Raised from 600 on 2026-08-14, because 600 covered fifteen seconds.** The
/// figure was sized against the consumer — the share `Intent` crosses a binder
/// transaction capped near a megabyte — and that reasoning is still right, but it
/// answered the wrong question: what a report needs is not *n lines*, it is
/// *enough time*. This client emits two messages per frame that nothing has yet
/// fixed ([`LOG_NOISE`]), so at 30 fps the tail reached back barely past the
/// moment it was trying to explain, and a freeze whose cause was several seconds
/// earlier was outside it. Ask for far more, drop the known repeats, and let
/// `tail_bytes` do the capping — the byte budget was always the real constraint.
const LOGCAT_TAIL_LINES: &str = "8000";

/// Per-frame log messages that are dropped from a shared report.
///
/// Both are real problems and neither is fixed by hiding it here; what is fixed is
/// a *report* in which they are 95% of the content. They are matched on substrings
/// rather than tags because both arrive twice — once from Bevy's `android_log`
/// layer and once through the stdout pump, under different tags.
///
/// - `could not set cursor position` — `bevy_winit` pushes `Window`'s cursor
///   position into winit whenever it changes, and winit's Android backend cannot
///   set one. `systems::touch` changes it every frame a finger moves, by design
///   (`docs/notes/touch-input.md`: the world-click consumers read the `Window`),
///   so the platform answers every real touch move with an `error!`.
/// - `Missing texture: Managed(` — `egui_wgpu` is asked to draw with a texture id
///   it does not hold. It has been firing continuously since the first run that
///   drew anything, so it is not new and it is not the freeze, but it is
///   undiagnosed and it should not be dropped from the log *itself*.
///
/// A count of what was removed goes in the report, so a reader is never quietly
/// shown a filtered log.
const LOG_NOISE: [&str; 2] = ["could not set cursor position", "Missing texture: Managed("];

/// The previous process's own log lines, read back out of the kernel log buffer.
///
/// This is the answer to the failure mode the crash reporter cannot otherwise
/// describe: a run that is *killed* rather than panicking leaves a breadcrumb
/// saying so and nothing else, because no Rust code runs at a `SIGSEGV` or an OOM
/// kill. But logd's ring buffer outlives the process, so the next launch can go
/// and read what the dead one said on its way out.
///
/// **An app may read its own log without `READ_LOGS`** — logd filters by UID, and
/// has since Android 4.1. So this returns our own lines and nothing else, which is
/// both the privacy story (nothing here belongs to another app) and the limit: a
/// low-memory kill is announced by `lowmemorykiller`/`ActivityManager` under the
/// *system* UID and is therefore invisible from in here. The evidence for one is
/// negative — our log simply stops, with no tombstone. A native crash is the
/// opposite: `crash_dump` reports under the crashing process's own UID, so the
/// `signal 11 (SIGSEGV)` block and its backtrace should be visible. *Needs a
/// device*: that second claim is the one to check against a real tombstone.
///
/// Failure is silent and returns `None`, which is correct for a diagnostic: it
/// runs on the startup path of an app that has already crashed once, and the
/// report it enriches is worth sending with or without it. Absolute path because a
/// NativeActivity process inherits the zygote's `PATH`, and the exec is plain
/// `fork`/`exec` rather than JNI — `logcat` is an ordinary binary in `/system/bin`
/// and needs no window, no VM attach and no permission.
pub fn own_logcat() -> Option<String> {
    let output = std::process::Command::new("/system/bin/logcat")
        .args(["-d", "-b", "main,crash", "-t", LOGCAT_TAIL_LINES])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (kept, dropped) = strip_log_noise(&text);
    if kept.trim().is_empty() {
        return None;
    }
    Some(if dropped == 0 {
        kept
    } else {
        format!("({dropped} repeated per-frame lines removed — see LOG_NOISE)\n{kept}")
    })
}

/// Drop every line containing one of [`LOG_NOISE`]; return what is left and how
/// many went.
///
/// Split out from [`own_logcat`] so it is testable off a device — the one part of
/// the crash reporter that has logic in it rather than a framework call.
fn strip_log_noise(text: &str) -> (String, usize) {
    let mut dropped = 0;
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let noisy = LOG_NOISE.iter().any(|needle| line.contains(needle));
            dropped += usize::from(noisy);
            !noisy
        })
        .collect();
    (kept.join("\n").trim().to_string(), dropped)
}

// ---------------------------------------------------------------------------
// 5. Why the last run ended — the framework already knows
// ---------------------------------------------------------------------------

/// `ApplicationExitInfo.REASON_ANR`.
const EXIT_REASON_ANR: i32 = 6;

/// How much of an ANR trace the report carries.
///
/// The trace is a dump of *every* thread in the process and runs to hundreds of
/// kilobytes on a real app. The **head** is what matters and the head is what this
/// keeps: `main` is the first thread in the dump, and `main` is the thread the ANR
/// is about. The ceiling is the binder transaction the share `Intent` crosses,
/// which is ~1 MB for everything in it together — the logcat tail is in there too.
const ANR_TRACE_MAX_BYTES: usize = 64 * 1024;

/// One line per recent process death, and — for an ANR — the thread dump.
///
/// **This is the instrument the previous two rounds needed and did not have.**
/// `docs/notes/android-build.md` says twice that an app cannot learn why it was
/// killed: the low-memory killer announces itself under the system UID, and the
/// ANR trace goes to tombstoned, so the evidence for either is *negative* — our
/// log simply stops. That was true of `logcat`. It is not true of the framework:
/// since API 30 `ActivityManager.getHistoricalProcessExitReasons` hands an app its
/// own recent deaths, each with a reason code, and for `REASON_ANR`
/// `getTraceInputStream()` returns **the very trace the system wrote to
/// tombstoned** — every thread, Java and native frames, with no permission and no
/// root.
///
/// So "the main loop stopped and the log cannot say where" is answerable after
/// all, and answerable from the share sheet on the next launch rather than from a
/// cable. The reason code alone settles the question the note calls unanswerable
/// — ANR, native crash, low memory and "the user swiped it away" are four
/// different codes and were previously one silence.
///
/// API-gated at 30 (`minSdkVersion` is 28), and every failure is silent: this
/// enriches a report that is worth sending with or without it.
pub fn recent_exit_reasons() -> Option<String> {
    let java_vm = vm().ok()?;
    let mut env = java_vm.attach_current_thread().ok()?;
    let activity = activity();
    match exit_reasons_inner(&mut env, &activity) {
        Ok(text) => text,
        Err(Fail(msg)) => {
            clear_pending(&env);
            tracing::warn!(error = %msg, "could not read the historical exit reasons");
            None
        }
    }
}

fn exit_reasons_inner(env: &mut JNIEnv, activity: &JObject) -> Result<Option<String>, Fail> {
    if sdk_int(env)? < 30 {
        return Ok(None);
    }
    // Not `with_local_frame` around the whole thing: the trace read below is a
    // loop, and a frame sized for the worst case is worse than frames sized for
    // each step. This one covers the list walk.
    // `with_local_frame`, not `with_local_frame_returning_local`: the latter exists
    // to promote a single **local reference** out of the frame being popped, so its
    // closure must return a `JObject`. This one returns `(Vec<String>,
    // Vec<GlobalRef>)` — strings already copied out of the JVM and *global* refs
    // that outlive any frame by construction — which is exactly the case the plain
    // form is for.
    let (summary, anr_infos) = env.with_local_frame::<_, _, Fail>(32, |env| {
        let name: JObject = env.new_string("activity")?.into();
        let manager = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        if manager.as_raw().is_null() {
            return Err(Fail("getSystemService(\"activity\") returned null".to_string()));
        }
        // `(null, 0, 6)`: our own package, any pid, the six most recent. Six spans
        // several install-test-crash cycles without turning the report into a
        // history book.
        let list = env
            .call_method(
                &manager,
                "getHistoricalProcessExitReasons",
                "(Ljava/lang/String;II)Ljava/util/List;",
                &[
                    JValue::Object(&JObject::null()),
                    JValue::Int(0),
                    JValue::Int(6),
                ],
            )?
            .l()?;
        let count = env.call_method(&list, "size", "()I", &[])?.i()?;
        let mut lines = Vec::new();
        let mut anr = Vec::new();
        for index in 0..count {
            let info = env
                .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(index)])?
                .l()?;
            if info.as_raw().is_null() {
                continue;
            }
            let reason = env.call_method(&info, "getReason", "()I", &[])?.i()?;
            let status = env.call_method(&info, "getStatus", "()I", &[])?.i()?;
            let pid = env.call_method(&info, "getPid", "()I", &[])?.i()?;
            let timestamp = env.call_method(&info, "getTimestamp", "()J", &[])?.j()?;
            let description = env
                .call_method(&info, "getDescription", "()Ljava/lang/String;", &[])?
                .l()?;
            let description = if description.as_raw().is_null() {
                String::new()
            } else {
                env.get_string(&JString::from(description))?.into()
            };
            lines.push(format!(
                "  pid {pid} — {} (reason {reason}, status {status}) at {timestamp} — {description}",
                reason_name(reason)
            ));
            // Only the newest ANR is worth a trace: the list is newest-first, so
            // the first one found is the death being reported.
            if reason == EXIT_REASON_ANR && anr.is_empty() {
                anr.push(env.new_global_ref(&info)?);
            }
        }
        Ok((lines, anr))
    })?;

    let mut out = String::from("--- how this app's recent processes ended (ActivityManager) ---\n");
    if summary.is_empty() {
        out.push_str("  (the framework has no record — this may be the first run)\n");
    }
    for line in &summary {
        out.push_str(line);
        out.push('\n');
    }
    if let Some(info) = anr_infos.first() {
        out.push_str(
            "\n--- the system's own ANR thread dump for that run (head; `main` is first) ---\n",
        );
        match read_trace(env, info.as_obj()) {
            Ok(Some(trace)) => out.push_str(&trace),
            Ok(None) => out.push_str("(the trace stream was empty)\n"),
            Err(Fail(msg)) => {
                clear_pending(env);
                out.push_str(&format!("(could not read the trace: {msg})\n"));
            }
        }
    }
    Ok(Some(out))
}

/// `ApplicationExitInfo`'s reason codes, spelled out. The number alone is a
/// lookup nobody holding a phone can perform.
fn reason_name(reason: i32) -> &'static str {
    match reason {
        1 => "EXIT_SELF (we asked to go)",
        2 => "SIGNALED",
        3 => "LOW_MEMORY (the OOM killer)",
        4 => "CRASH (an unhandled Java exception)",
        5 => "CRASH_NATIVE (a signal — SIGSEGV and friends)",
        EXIT_REASON_ANR => "ANR (the system decided we stopped responding)",
        7 => "INITIALIZATION_FAILURE",
        8 => "PERMISSION_CHANGE",
        9 => "EXCESSIVE_RESOURCE_USAGE",
        10 => "USER_REQUESTED (swiped away)",
        11 => "USER_STOPPED",
        12 => "DEPENDENCY_DIED",
        13 => "OTHER",
        14 => "FREEZER",
        15 => "PACKAGE_STATE_CHANGE",
        16 => "PACKAGE_UPDATED",
        _ => "unknown reason",
    }
}

/// Drain `ApplicationExitInfo.getTraceInputStream()` into a `String`, capped.
fn read_trace(env: &mut JNIEnv, info: &JObject) -> Result<Option<String>, Fail> {
    // The error type is spelled out because nothing in the closure body pins it:
    // every `?` here converts *into* `E`, so `E` is only constrained by the return,
    // and `Ok(..)` alone leaves it ambiguous. This is the one call in the file where
    // `returning_local` is the right choice — a single local `JObject` promoted out
    // of the frame — so it keeps it and gains the turbofish.
    let stream = env.with_local_frame_returning_local::<_, Fail>(8, |env| {
        Ok(env
            .call_method(info, "getTraceInputStream", "()Ljava/io/InputStream;", &[])?
            .l()?)
    })?;
    if stream.as_raw().is_null() {
        return Ok(None);
    }
    let mut bytes: Vec<u8> = Vec::new();
    env.with_local_frame::<_, _, Fail>(8, |env| {
        // One buffer for the whole read: allocating it inside the loop would put a
        // local reference per chunk into a frame sized for none of them, which on a
        // 400 KB trace is the 512-entry table overflow that aborts the process —
        // the failure this file's module note opens with.
        let buf = env.new_byte_array(8192)?;
        loop {
            let read = env
                .call_method(&stream, "read", "([B)I", &[JValue::Object(&buf)])?
                .i()?;
            if read <= 0 {
                break;
            }
            let mut chunk = vec![0i8; read as usize];
            env.get_byte_array_region(&buf, 0, &mut chunk)?;
            bytes.extend(chunk.iter().map(|b| *b as u8));
            if bytes.len() >= ANR_TRACE_MAX_BYTES {
                break;
            }
        }
        // Best-effort: a leaked stream lives until the process ends, and the
        // process is about to have a very short life either way.
        let _ = env.call_method(&stream, "close", "()V", &[]);
        Ok(())
    })?;
    if bytes.is_empty() {
        return Ok(None);
    }
    bytes.truncate(ANR_TRACE_MAX_BYTES);
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

// ---------------------------------------------------------------------------
// 1. Location
// ---------------------------------------------------------------------------

const PERM_FINE: &str = "android.permission.ACCESS_FINE_LOCATION";
const PERM_COARSE: &str = "android.permission.ACCESS_COARSE_LOCATION";
/// `PackageManager.PERMISSION_GRANTED`.
const PERMISSION_GRANTED: i32 = 0;
/// Any non-negative value; `Activity.requestPermissions` rejects negatives. We
/// never see the result code, since the callback that would carry it is a Java
/// override we cannot supply.
const PERMISSION_REQUEST_CODE: i32 = 0x5645;
/// `PendingIntent.FLAG_MUTABLE`.
///
/// **Measured, not chosen** (device log, 2026-08-14). This was `FLAG_IMMUTABLE`,
/// on the reasoning that the intent is never consumed — we register it to make the
/// provider *run*, and the `Location` extra it carries goes to a broadcast with no
/// receiver — so the security-correct flag cost us nothing that the design was not
/// already giving up. The documented failure mode agreed: an immutable intent
/// simply arrives with a null extra.
///
/// The framework does not agree. `LocationManager.requestLocationUpdates` opens
/// with `Preconditions.checkArgument(pendingIntent.isMutable(), "pending intent
/// must be mutable")`, so the immutable one threw
/// `IllegalArgumentException` before any registration happened at all — and
/// because the failure is deliberately non-fatal here, the app fell back to
/// polling a cache that nothing was refreshing. That is an arrow that never
/// appears and never moves, with one `System.err` line to say why.
///
/// One of the two mutability flags is mandatory from API 31 (neither is an
/// `IllegalArgumentException` of its own) and both are harmless below it, so this
/// is not a version-gated choice. The intent stays explicit-by-package
/// ([`request_updates`]), which is what keeps a mutable intent from being an
/// invitation to the rest of the phone.
const PENDING_INTENT_FLAG_MUTABLE: i32 = 0x0200_0000;

/// Providers we will use, best first.
///
/// `fused` is the **platform's** `LocationManager.FUSED_PROVIDER`, not Google Play
/// Services' `FusedLocationProviderClient` — the latter arrives as an AAR and is
/// therefore a Gradle project, i.e. out of reach for this app by construction. It
/// is only public API from API 31, so rather than gate on the SDK level we take
/// whatever the platform itself lists as enabled (see [`enabled_providers`]) and
/// intersect: a name the framework just told us about cannot be one it rejects.
const PROVIDERS: [&str; 3] = ["fused", "gps", "network"];

/// How often the cached fix is re-read.
///
/// **This was 1000 ms, on the reasoning that GNSS reports at ~1 Hz so polling
/// faster buys nothing but JNI calls. That is wrong, and it costs half a second.**
/// Our poll and the provider's stream are two unsynchronised 1 Hz clocks: a fix
/// written to the cache just after a poll is not read until the next one, so the
/// latency added by the polling alone is uniform on 0–1000 ms — a mean of 500 ms
/// — and it *beats*, so the arrow moves in an irregular pattern of one-second and
/// two-second steps rather than a steady tick. Reading four times per provider
/// interval brings the added mean latency to ~125 ms and makes the step rate the
/// receiver's own.
///
/// The cost is real and small: one `getLastKnownLocation` per usable provider per
/// tick (three at most, see [`PROVIDERS`]) plus one `elapsedRealtimeNanos`, on a
/// thread that is attached to the JVM for its whole life precisely so a poll is
/// not an attach. It runs only while something holds a [`LocationWatch`].
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long we keep asking `checkSelfPermission` after putting the dialog up.
const PERMISSION_WAIT: Duration = Duration::from_secs(120);
/// The **first** fix we publish must be at most this old. A cached position from
/// this morning is not "where the device is"; it is where it was parked, and an
/// arrow placed there is worse than an arrow that has not appeared yet. Later fixes
/// need only be newer than the last published one.
///
/// Ten minutes, not the one it was. A cold GNSS acquisition is tens of seconds, and
/// refusing everything older than a minute meant a phone that had a perfectly
/// serviceable two-minute-old fused fix in hand still showed "Waiting for GPS…" for
/// most of a minute after Start — the single loudest difference between this and
/// the reference apps. What makes the wider window safe is that staleness is no
/// longer hidden: [`stale_accuracy_m`] folds the fix's age into its reported
/// accuracy, so a two-minute-old position arrives as a *poor* one and every
/// consumer that already reads `accuracy_m` — the GPS chip, the reroute's
/// `is_uncertain` guard — treats it as the guess it is.
const FIRST_FIX_MAX_AGE: Duration = Duration::from_secs(600);

/// How fast we assume a device might have moved since a fix was taken, in metres
/// per second, when inflating a stale fix's accuracy.
///
/// 13 m/s is a shade under 50 km/h: an urban drive. It deliberately over-states a
/// pedestrian and under-states a motorway, because the number's job is to make a
/// stale fix *read* as uncertain rather than to bound the error — the ceiling on
/// how wrong it can be is [`FIRST_FIX_MAX_AGE`], not this.
const STALE_DRIFT_MS: f64 = 13.0;

/// A fix's reported accuracy, widened by how long ago it was taken.
///
/// The alternative — publishing an old position at its original accuracy — is the
/// one thing a navigation display must not do: it is not wrong about where you
/// are, it is *confident* about it.
#[must_use]
fn stale_accuracy_m(accuracy_m: f64, age_s: f64) -> f64 {
    accuracy_m + age_s.max(0.0) * STALE_DRIFT_MS
}
/// Requested update period, milliseconds. This is a *hint* to the provider and the
/// framework is free to be slower.
///
/// 500 ms rather than the 1000 it was. GNSS hardware on a current handset runs at
/// several hertz and the platform reports at whatever the most demanding client
/// asked for, so asking for 1 Hz was asking for the floor and getting it — while
/// what this drives is a moving arrow, where the difference between one update a
/// second and two is the difference between a step and a slide. Asking for much
/// less than the hardware can give would only be answered at the hardware's rate
/// anyway, so this is not a bid for a rate nobody has; it is not asking for the
/// slowest one on offer.
const UPDATE_INTERVAL_MS: i64 = 500;

/// Set while the system's runtime-permission dialog is up.
///
/// **This exists to keep two threads off the activity's `Window` at once, and
/// that is not hygiene.** `Activity.requestPermissions` is reached from the
/// location worker, and its AOSP implementation ends in
/// `startActivityForResult` → `cancelInputsAndStartExitTransition`, which reads
/// `mWindow.peekDecorView()` and walks the view hierarchy cancelling pending
/// input — a UI-thread structure, touched from ours. `open_url` in this same file
/// documents that hazard and sidesteps it by starting from the *application*
/// context; the permission dialog has no such escape, because there is no
/// dex-free way to post a `Runnable` to the UI thread (see the module note).
///
/// What we can do is not make it worse. Driver mode turns on at the same instant
/// it asks for the permission, and `apply_screen_wakelock` answers that by writing
/// `FLAG_KEEP_SCREEN_ON` — which the framework performs on the UI thread, on the
/// same `Window`, while the framework is also tearing that window's input channel
/// down for the incoming dialog. Three writers on one window is not a race worth
/// having for a flag that can just as well be set a moment later, so the wakelock
/// waits on this.
static PERMISSION_DIALOG_UP: AtomicBool = AtomicBool::new(false);

/// Whether a runtime-permission dialog is up — see [`PERMISSION_DIALOG_UP`].
///
/// Always `false` before anything asks for a permission and after the answer
/// lands, so a caller that reads it on every frame is reading "no" almost always.
#[must_use]
pub fn permission_dialog_up() -> bool {
    PERMISSION_DIALOG_UP.load(Ordering::Relaxed)
}

/// Distinct request codes per subscription, so two overlapping `LocationWatch`es
/// cannot cancel each other. `PendingIntent` identity includes the request code, so
/// a stopping watch's `removeUpdates` then names only its own registration —
/// without this, a watch started before the previous one's thread had finished
/// unwinding would be silently unsubscribed by its predecessor.
static NEXT_REQUEST_CODE: AtomicI32 = AtomicI32::new(0x5646);

/// A live location subscription. Held inside `LocationWatch`; dropping that calls
/// [`Self::stop`].
pub struct LocationSubscription {
    cancel: Arc<AtomicBool>,
}

impl LocationSubscription {
    /// End the subscription.
    ///
    /// This **signals and returns**; it does not join. The worker is mid-`sleep`
    /// most of the time and joining would block whoever dropped the watch — which
    /// is a Bevy system on the render thread — for up to a slice. The worker checks
    /// the flag every 100 ms and calls `removeUpdates` on its way out, so the radio
    /// is released promptly even though the release is not synchronous with this
    /// call.
    pub fn stop(self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Begin watching the device's position. The counterpart of `wasm::start`.
pub fn start_location(slot: Slot) -> Option<LocationSubscription> {
    if ANDROID_APP.get().is_none() {
        report(&slot, Err("no Android activity".to_string()));
        return None;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let worker_slot = slot.clone();
    // A dedicated thread, not a Bevy system: attaching and detaching from the JVM
    // costs far more than the work, the fix rate is ~1 Hz, and a permission dialog
    // has to be waited out for up to two minutes — none of which belongs in a
    // schedule that runs sixty times a second.
    let spawned = std::thread::Builder::new()
        .name("cartopolis-location".to_string())
        .spawn(move || {
            if let Err(msg) = run_location(&worker_slot, &worker_cancel) {
                tracing::warn!(error = %msg, "android location provider stopped");
                report(&worker_slot, Err(msg));
            }
        });
    match spawned {
        // Detached deliberately: the join handle would only let us block on a
        // thread we have already told to stop. `cancel` is the whole protocol.
        Ok(_handle) => Some(LocationSubscription { cancel }),
        Err(e) => {
            report(
                &slot,
                Err(format!("could not start the location thread: {e}")),
            );
            None
        }
    }
}

/// Write into the shared slot. Kept in one place so the rule below is enforceable.
///
/// **Only terminal failures are reported as `Err`.** The slot holds one value, so
/// an error erases the last good fix — which for driver mode means the arrow
/// disappearing. A denied permission or a missing provider is worth that; a
/// transient JNI hiccup in the poll loop is not, and is logged instead.
fn report(slot: &Slot, value: Result<GpsFix, String>) {
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(value);
    }
}

/// Sleep `total`, waking every 100 ms to notice a cancellation. Returns `false` if
/// the subscription was stopped.
fn nap(cancel: &AtomicBool, total: Duration) -> bool {
    const SLICE: Duration = Duration::from_millis(100);
    let mut left = total;
    while !left.is_zero() {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let step = left.min(SLICE);
        std::thread::sleep(step);
        left -= step;
    }
    !cancel.load(Ordering::Relaxed)
}

/// Raises [`PERMISSION_DIALOG_UP`] for as long as it is alive.
///
/// A guard rather than a pair of stores because the wait it spans has five exits
/// — granted, denied, cancelled, a JNI failure, and the `?` on the request itself
/// — and one of them forgetting to lower the flag is a permanent one.
struct PermissionDialog;

impl PermissionDialog {
    fn raise() -> Self {
        PERMISSION_DIALOG_UP.store(true, Ordering::Relaxed);
        Self
    }
}

impl Drop for PermissionDialog {
    fn drop(&mut self) {
        PERMISSION_DIALOG_UP.store(false, Ordering::Relaxed);
    }
}

/// The outcome of asking for a runtime permission.
enum Permission {
    Granted,
    Denied,
    /// The caller cancelled while the dialog was up.
    Cancelled,
}

/// The microphone, for [`ensure_permission`]. `platform::audio`'s Android arm is
/// the only caller.
pub const PERM_RECORD_AUDIO: &str = "android.permission.RECORD_AUDIO";

/// The camera, for calls (`platform::camera`) and — because declaring it makes
/// holding it a condition of `ACTION_IMAGE_CAPTURE` — for a place photo too.
pub const PERM_CAMERA: &str = "android.permission.CAMERA";

/// Ask for one runtime permission and block until it is answered.
///
/// The generic form of what location has always done here, exposed because the
/// microphone needs exactly the same dance and reimplementing it would mean two
/// copies of the polling wait, the dialog guard and the local-frame discipline.
///
/// **Blocks for up to [`PERMISSION_WAIT`] (two minutes), so call it from a
/// worker thread and never from a Bevy system.** There is no callback to wait on
/// — `onRequestPermissionsResult` is a Java override this dex-free app cannot
/// supply — so the answer is polled, exactly as `settle_permission` documents.
/// `cancel` is checked every 100 ms, which is how a caller that has given up (the
/// user closed the panel) stops paying for the wait.
///
/// `Err` is a *structural* failure — no activity, no JVM, a JNI call that threw —
/// and is distinct from a denial: one is worth showing the user as "denied,
/// here's how to change it" and the other is a bug in this file.
pub fn ensure_permission(permission: &str, cancel: &AtomicBool) -> Result<bool, String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;
    let activity = activity();
    // The short name is what the log wants: the caller passes a fully-qualified
    // constant, and `android.permission.RECORD_AUDIO` in a line that already says
    // "permission" three times reads worse than "RECORD_AUDIO".
    let what = permission.rsplit('.').next().unwrap_or(permission);
    match settle_permission(&mut env, &activity, cancel, &[permission], what) {
        Ok(Permission::Granted) => Ok(true),
        Ok(Permission::Denied | Permission::Cancelled) => Ok(false),
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

/// The worker thread.
///
/// ## Why this shape, and what was rejected
///
/// The natural Android call is
/// `LocationManager.requestLocationUpdates(String, long, float, LocationListener,
/// Looper)`, and it is unreachable: `LocationListener` is a Java interface, so
/// satisfying it means a class in a `classes.dex`, which is the one thing this app
/// does not have (`docs/notes/android-build.md`). The same objection kills every
/// other push-shaped API in the package — `getCurrentLocation` needs a
/// `java.util.function.Consumer`, `registerGnssStatusCallback` a
/// `GnssStatus.Callback`, `addNmeaListener` an `OnNmeaMessageListener`. So does the
/// clever-sounding escape: `java.lang.reflect.Proxy` synthesises an implementation
/// of an interface at runtime, but it needs an `InvocationHandler`, which is itself
/// an interface. Loading hand-authored dex bytes through `InMemoryDexClassLoader`
/// would work and is rejected on the note's own grounds — it is Java bytecode we
/// author and `d8` compiles, dressed up as a byte array.
///
/// What *is* reachable with no listener is the pair below, and they only work
/// together:
///
/// 1. `requestLocationUpdates(String, long, float, PendingIntent)` — the broadcast
///    overload. `PendingIntent.getBroadcast` is a static framework method, and the
///    broadcast it names has **no receiver**, deliberately: a `<receiver>` in the
///    manifest points at a class. The delivery therefore goes nowhere. The
///    registration is not for the data, it is to make the provider *run*.
/// 2. `getLastKnownLocation(provider)`, polled. The framework caches the most
///    recent fix per provider and refreshes that cache from any fix the provider
///    reports, whoever asked for it — including ours.
///
/// ### What is guaranteed and what is inference
///
/// *Guaranteed by the documented contract:* `getLastKnownLocation` returns the last
/// fix obtained from the provider and does not itself start the provider (so
/// polling alone would return a stale position, or null, whenever nothing else on
/// the phone is using GNSS — this is exactly why step 1 exists); an active
/// `requestLocationUpdates` registration starts the provider; a broadcast with no
/// receivers is a no-op rather than an error; `PendingIntent.send` reports
/// `CanceledException` only for a cancelled intent or a dead owner, neither of
/// which a live foreground app is.
///
/// *Inference that needs a device to confirm* — four claims, in falling order of
/// confidence:
///
/// - **The provider's fixes reach the cache our poll reads.** AOSP's
///   `LocationProviderManager.onReportLocation` updates the per-provider last
///   location on every report, independent of which client's registration caused
///   it. If this turned out to be per-client, the poll would return the same
///   position forever and the symptom would be an arrow that never moves — which is
///   the first thing to check on a device.
/// - ~~**An immutable `PendingIntent` keeps the registration alive.**~~ **Settled
///   on hardware, 2026-08-14, and it was wrong.** The reasoning was that an
///   unconsumed intent should take the security-correct flag and that the
///   documented cost — a null `Location` extra in a receiver we do not have — was
///   already ours. `LocationManager` does not offer that trade:
///   `requestLocationUpdates` asserts `pendingIntent.isMutable()` up front and
///   throws. The registration never existed, the provider was never started, and
///   the poll below read a cache nothing was refreshing. See
///   [`PENDING_INTENT_FLAG_MUTABLE`]. The line that named it was one `System.err`
///   stack trace in the log tail the crash reporter shares, which is the whole
///   argument for that instrument.
/// - **Foreground rate.** Android 8's background location throttling (a few fixes
///   an hour) does not apply to a foreground app. This app is only ever foreground
///   while it renders, so the expectation is the requested ~1 Hz.
/// - **`Activity.requestPermissions` from this thread.** See
///   [`request_permission`].
///
/// If the first claim fails on a device, the fallback with the best chance is
/// `getCurrentLocation` on API 30+ — which needs a `Consumer`, and therefore needs
/// the GameActivity/Gradle move the note already names as the trigger for taking on
/// Java at all. There is no third option that stays dex-free.
fn run_location(slot: &Slot, cancel: &AtomicBool) -> Result<(), String> {
    // Breadcrumbs, one per stage, and they earn their place: this worker is
    // reached by tapping *Drive*, which is also the first thing in the app's life
    // that raises a system dialog — and a phone that freezes at that moment
    // offers no other way to tell "the JNI call threw", "the user is still
    // looking at the dialog", "no provider is on" and "we never came back from
    // being backgrounded" apart. There is no cable and no debugger; the log
    // excerpt the crash reporter shares on the next launch is the instrument.
    tracing::info!("location worker starting");
    let java_vm = vm()?;
    // Attached for the thread's whole life and detached by dropping this guard on
    // the way out. Per-call attach/detach would be the dominant cost of a 1 Hz
    // poll, and a permanent attach would leave the thread attached after it exits.
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach the location thread to the JVM: {e}"))?;
    let activity = activity();

    match settle_permission(
        &mut env,
        &activity,
        cancel,
        &[PERM_FINE, PERM_COARSE],
        "location",
    ) {
        Ok(Permission::Granted) => {}
        Ok(Permission::Denied) => {
            // Terminal, and the one message the driver-mode UI most needs: without
            // it the panel says "waiting for a fix" forever for a fix that is never
            // coming.
            report(slot, Err("Location permission denied".to_string()));
            return Ok(());
        }
        Ok(Permission::Cancelled) => return Ok(()),
        Err(Fail(msg)) => {
            clear_pending(&env);
            return Err(msg);
        }
    }

    let manager = match location_manager(&mut env, &activity) {
        Ok(m) => m,
        Err(Fail(msg)) => {
            clear_pending(&env);
            return Err(msg);
        }
    };

    let available = match enabled_providers(&mut env, &manager) {
        Ok(list) => list,
        Err(Fail(msg)) => {
            clear_pending(&env);
            return Err(msg);
        }
    };
    // Preference order, intersected with what the platform says is on.
    let usable: Vec<&'static str> = PROVIDERS
        .into_iter()
        .filter(|wanted| available.iter().any(|have| have.as_str() == *wanted))
        .collect();
    if usable.is_empty() {
        report(
            slot,
            Err("No location provider is enabled — turn Location on".to_string()),
        );
        return Ok(());
    }
    tracing::info!(providers = ?usable, "android location providers");

    // **Register on every usable provider, not just the best one.** This used to
    // take `usable[0]` alone — `fused`, on the argument that it already blends
    // GNSS, wifi and cell and that a second registration is a second radio kept
    // warm. True, and it costs the thing driver mode is judged on: the fused
    // provider decides for itself when to spin GNSS up, and asked for a 1 Hz
    // stream it will happily answer from the network for the first half minute.
    // Naming `gps` explicitly starts the acquisition at the moment the user
    // pressed Start rather than whenever the blend gets round to it. The radios go
    // straight back down when the last `LocationWatch` drops.
    let mut pending: Vec<GlobalRef> = Vec::new();
    for provider in &usable {
        let request_code = NEXT_REQUEST_CODE.fetch_add(1, Ordering::Relaxed);
        match request_updates(&mut env, &activity, &manager, provider, request_code) {
            Ok(pi) => pending.push(pi),
            Err(Fail(msg)) => {
                // Not fatal. Without any registration we are reduced to whatever
                // fix some other app on the phone happens to produce — a genuinely
                // worse but still working provider, and much better than no driver
                // mode at all.
                clear_pending(&env);
                tracing::warn!(provider, error = %msg, "requestLocationUpdates failed for this provider");
            }
        }
    }
    if pending.is_empty() {
        tracing::warn!(
            "no location registration succeeded — falling back to polling the cached fix only"
        );
    }

    // We *poll* every usable provider, not just the one we requested. A cold GNSS
    // start is tens of seconds while `network` usually has something within two, so
    // reading only the preferred provider would leave the arrow absent for the part
    // of the drive where it is most wanted. Freshness decides between them, so a
    // coarse fix is never preferred to a newer fine one.
    let mut last_nanos = i64::MIN;
    let mut published = false;
    while !cancel.load(Ordering::Relaxed) {
        let now = match elapsed_realtime_nanos(&mut env) {
            Ok(n) => n,
            Err(Fail(msg)) => {
                clear_pending(&env);
                tracing::warn!(error = %msg, "SystemClock.elapsedRealtimeNanos failed");
                if !nap(cancel, POLL_INTERVAL) {
                    break;
                }
                continue;
            }
        };

        let mut best: Option<Reading> = None;
        for &provider in &usable {
            match read_provider(&mut env, &manager, provider) {
                Ok(Some(reading)) => {
                    if best.as_ref().is_none_or(|b| reading.nanos > b.nanos) {
                        best = Some(reading);
                    }
                }
                Ok(None) => {}
                Err(Fail(msg)) => {
                    clear_pending(&env);
                    // Transient by assumption, so logged rather than reported —
                    // see `report`'s note on why an Err would erase a good fix.
                    tracing::warn!(provider, error = %msg, "getLastKnownLocation failed");
                }
            }
        }

        if let Some(reading) = best {
            // `max(0)`: a fix stamped microseconds ahead of our own clock read is
            // fresh, not impossibly old — without the clamp the negative age casts
            // to a huge `u128` and the very first fix is rejected forever.
            let age_ns = now.saturating_sub(reading.nanos).max(0) as u128;
            let fresh_enough = published || age_ns < FIRST_FIX_MAX_AGE.as_nanos();
            if reading.nanos > last_nanos && fresh_enough {
                let mut reading = reading;
                reading.fix.accuracy_m =
                    stale_accuracy_m(reading.fix.accuracy_m, (age_ns as f64) / 1_000_000_000.0);
                if !published {
                    // The one line that separates "the receiver is still cold"
                    // from "the cache our poll reads is per-client after all",
                    // which is the load-bearing inference in this file's design
                    // (see `run_location`) and the thing a device has to settle.
                    tracing::info!(
                        accuracy_m = reading.fix.accuracy_m,
                        age_ms = (age_ns / 1_000_000) as u64,
                        "first location fix"
                    );
                }
                last_nanos = reading.nanos;
                published = true;
                report(slot, Ok(reading.fix));
            }
        }

        if !nap(cancel, POLL_INTERVAL) {
            break;
        }
    }

    for pending in &pending {
        // Releasing the radio is the whole reason a watch is a subscription rather
        // than a global. Best-effort: the thread is on its way out and there is
        // nobody left to report to.
        if let Err(Fail(msg)) = remove_updates(&mut env, &manager, pending) {
            clear_pending(&env);
            tracing::warn!(error = %msg, "removeUpdates failed");
        }
    }
    tracing::info!("android location watch stopped");
    Ok(())
}

/// One `Location`, plus the monotonic timestamp used to tell fixes apart.
struct Reading {
    fix: GpsFix,
    /// `Location.getElapsedRealtimeNanos` — nanoseconds since boot, *including*
    /// sleep. Deliberately not `getTime()`, which is wall-clock and therefore
    /// jumps backwards when the network corrects the phone's clock; a fix would
    /// then look older than one it followed and never be published again.
    nanos: i64,
}

// --- permission ------------------------------------------------------------

/// Ask for `names` (any one of which is enough) and wait for the answer.
///
/// `what` names the capability in the log — "location", "microphone" — because
/// the interesting line is not that *a* permission was asked for but which one,
/// on a platform where the log excerpt shared after a freeze is the whole
/// instrument.
fn settle_permission(
    env: &mut JNIEnv,
    activity: &JObject,
    cancel: &AtomicBool,
    names: &[&str],
    what: &str,
) -> Result<Permission, Fail> {
    if permission_granted(env, activity, names)? {
        tracing::info!("{what} permission already granted");
        return Ok(Permission::Granted);
    }
    // The dialog backgrounds the activity. That is expected and is why this line
    // is here: a freeze that follows it is a *lifecycle* failure, not a location
    // one, and the two are indistinguishable without knowing whether we ever
    // asked (see `quiesce_while_backgrounded`, which logs the other half).
    tracing::info!("asking for the {what} permission — the system dialog is about to appear");
    // Raised *before* the call and lowered on every path out of the wait below
    // (`PermissionDialog`'s `Drop`), including the `?` — see [`PERMISSION_DIALOG_UP`]
    // for what reads it. A flag left raised would hold the screen-wakelock write
    // off for the rest of the run.
    let _dialog = PermissionDialog::raise();
    request_permission(env, activity, names)?;
    // **Polling, because the answer is unreachable.** The result arrives at
    // `Activity.onRequestPermissionsResult`, which is a Java method we would have
    // to override — a class, and therefore out. `Context.checkSelfPermission` is a
    // plain framework call that reads the current grant, so asking it repeatedly
    // gets the same information a callback would, a beat later. The user may sit on
    // the dialog, or wander into Settings, hence two minutes rather than seconds.
    let steps = PERMISSION_WAIT.as_millis() / 500;
    for step in 0..steps {
        if !nap(cancel, Duration::from_millis(500)) {
            tracing::info!("{what} request dropped while the permission dialog was up");
            return Ok(Permission::Cancelled);
        }
        if permission_granted(env, activity, names)? {
            // `as u64`: `tracing`'s field values do not cover `u128`, which is
            // what `Duration::as_millis` arithmetic produces.
            tracing::info!(
                waited_ms = ((step + 1) * 500) as u64,
                "{what} permission granted"
            );
            return Ok(Permission::Granted);
        }
    }
    tracing::info!("{what} permission never granted — giving up");
    Ok(Permission::Denied)
}

/// Whether we hold *any* of `names`.
///
/// Any, not all, and that is the location case: coarse alone is accepted rather
/// than treated as a denial, because it yields a real fix with an honest
/// `accuracy_m` of a kilometre or two, which the caller can judge, and refusing
/// it would turn "the user picked Approximate" into "location is broken". A
/// single-element slice (the microphone) reads the same way.
fn permission_granted(env: &mut JNIEnv, activity: &JObject, names: &[&str]) -> Result<bool, Fail> {
    env.with_local_frame(8, |env| {
        for name in names {
            let perm: JObject = env.new_string(name)?.into();
            let result = env
                .call_method(
                    activity,
                    "checkSelfPermission",
                    "(Ljava/lang/String;)I",
                    &[JValue::Object(&perm)],
                )?
                .i()?;
            if result == PERMISSION_GRANTED {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

/// Put the system permission dialog up.
///
/// `Activity.requestPermissions` is a *framework* method on a class we did not
/// write, so it needs no dex — but it is the one call in this file made from a
/// thread the framework did not intend. AOSP's implementation reads and writes
/// `mHasCurrentPermissionsRequest` and calls `startActivityForResult`, neither
/// synchronised; from Bevy's thread that is a data race on two `Activity` fields
/// rather than anything that touches a view. The alternative is to post it to the
/// UI thread, which needs a `Runnable`, which needs a class — so there is no
/// dex-free version of "do this properly", and the choice is between this and no
/// runtime permission at all.
///
/// *Needs a device.* It is called at most once per watch, from one thread, which is
/// the narrowest the race can be made without leaving the constraint. If it
/// misbehaves the symptom would be the dialog failing to appear at all, after which
/// `settle_permission` reports a denial two minutes later — noisy, but not a crash.
fn request_permission(env: &mut JNIEnv, activity: &JObject, names: &[&str]) -> Result<(), Fail> {
    // One local reference per name plus the array, inside a frame sized for the
    // largest call site with slack — the local-reference table aborts the process
    // at 512 entries and this is called from a loop-free path only by luck.
    let frame = (names.len() as i32 + 1).max(8);
    env.with_local_frame(frame, |env| {
        let null = JObject::null();
        let array = env.new_object_array(names.len() as i32, "java/lang/String", &null)?;
        for (i, name) in names.iter().enumerate() {
            let value: JObject = env.new_string(name)?.into();
            env.set_object_array_element(&array, i as i32, &value)?;
        }
        let names: JObject = array.into();
        env.call_method(
            activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&names), JValue::Int(PERMISSION_REQUEST_CODE)],
        )?;
        Ok(())
    })
}

// --- the LocationManager ---------------------------------------------------

/// `Context.getSystemService(Context.LOCATION_SERVICE)`, kept as a global reference
/// so it outlives the per-poll local frames.
fn location_manager(env: &mut JNIEnv, activity: &JObject) -> Result<GlobalRef, Fail> {
    env.with_local_frame(8, |env| {
        // `Context.LOCATION_SERVICE`. A static-field read would need the field's
        // class and its type; the value has been "location" since API 1 and is
        // part of the public contract.
        let name: JObject = env.new_string("location")?.into();
        let service = env
            .call_method(
                activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        if service.as_raw().is_null() {
            return Err(Fail(
                "getSystemService(\"location\") returned null".to_string(),
            ));
        }
        Ok(env.new_global_ref(&service)?)
    })
}

/// `LocationManager.getProviders(true)` — the providers the platform says are
/// currently enabled, as plain strings.
fn enabled_providers(env: &mut JNIEnv, manager: &GlobalRef) -> Result<Vec<String>, Fail> {
    env.with_local_frame(24, |env| {
        let list = env
            .call_method(
                manager.as_obj(),
                "getProviders",
                "(Z)Ljava/util/List;",
                &[JValue::Bool(u8::from(true))],
            )?
            .l()?;
        let count = env.call_method(&list, "size", "()I", &[])?.i()?;
        let mut names = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            let item = env
                .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(index)])?
                .l()?;
            if item.as_raw().is_null() {
                continue;
            }
            let text: String = env.get_string(&JString::from(item))?.into();
            names.push(text);
        }
        Ok(names)
    })
}

/// Register for updates so the provider actually runs. The broadcast is never
/// received — see [`run_location`].
fn request_updates(
    env: &mut JNIEnv,
    activity: &JObject,
    manager: &GlobalRef,
    provider: &str,
    request_code: i32,
) -> Result<GlobalRef, Fail> {
    // The closure's error type is annotated, and has to be. `with_local_frame` is
    // generic over `E: From<jni::errors::Error>`, and there are *two* such impls in
    // this build — ours below, and `android_activity`'s own `InternalAppError` —
    // so inference has no way to pick one and every `?` in the body becomes
    // ambiguous. It surfaces as a dozen `type annotations needed` errors all
    // pointing at the closure's final expression rather than at the cause.
    let pending = env.with_local_frame::<_, _, Fail>(12, |env| {
        let action: JObject = env
            .new_string("eu.garage44.cartopolis.LOCATION_TICK")?
            .into();
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action)],
        )?;
        // Explicit by package. Android 8 blocks most *implicit* broadcasts from
        // manifest-registered receivers, and while ours has no receiver at all,
        // an implicit broadcast is also an invitation to every app on the phone.
        // Asking the context for its own name beats hardcoding one that has to
        // stay in step with the manifest.
        let package = env
            .call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?
            .l()?;
        env.call_method(
            &intent,
            "setPackage",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&package)],
        )?;
        let pending = env
            .call_static_method(
                "android/app/PendingIntent",
                "getBroadcast",
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                &[
                    JValue::Object(activity),
                    JValue::Int(request_code),
                    JValue::Object(&intent),
                    JValue::Int(PENDING_INTENT_FLAG_MUTABLE),
                ],
            )?
            .l()?;
        Ok(env.new_global_ref(&pending)?)
    })?;

    env.with_local_frame::<_, _, Fail>(8, |env| {
        let name: JObject = env.new_string(provider)?.into();
        env.call_method(
            manager.as_obj(),
            "requestLocationUpdates",
            "(Ljava/lang/String;JFLandroid/app/PendingIntent;)V",
            &[
                JValue::Object(&name),
                JValue::Long(UPDATE_INTERVAL_MS),
                // Zero metres: a distance filter would suppress updates while the
                // vehicle is stopped, and a stopped vehicle is exactly when the
                // heading and speed need to decay to zero rather than hold.
                JValue::Float(0.0),
                JValue::Object(pending.as_obj()),
            ],
        )?;
        Ok(())
    })?;
    tracing::info!(provider, "requested location updates");
    Ok(pending)
}

fn remove_updates(env: &mut JNIEnv, manager: &GlobalRef, pending: &GlobalRef) -> Result<(), Fail> {
    env.with_local_frame(4, |env| {
        env.call_method(
            manager.as_obj(),
            "removeUpdates",
            "(Landroid/app/PendingIntent;)V",
            &[JValue::Object(pending.as_obj())],
        )?;
        Ok(())
    })
}

fn elapsed_realtime_nanos(env: &mut JNIEnv) -> Result<i64, Fail> {
    env.with_local_frame(4, |env| {
        Ok(env
            .call_static_method("android/os/SystemClock", "elapsedRealtimeNanos", "()J", &[])?
            .j()?)
    })
}

/// Read one provider's cached fix, or `None` when it has never had one.
///
/// Every optional field is read through its `has*` predicate and left `None` when
/// absent. `Location`'s getters return `0.0` for a value the device could not
/// derive, and a course of 0° is due north — publishing that as a heading would
/// point the driver-mode arrow north every time the vehicle stopped.
fn read_provider(
    env: &mut JNIEnv,
    manager: &GlobalRef,
    provider: &str,
) -> Result<Option<Reading>, Fail> {
    env.with_local_frame(12, |env| {
        let name: JObject = env.new_string(provider)?.into();
        let location = env
            .call_method(
                manager.as_obj(),
                "getLastKnownLocation",
                "(Ljava/lang/String;)Landroid/location/Location;",
                &[JValue::Object(&name)],
            )?
            .l()?;
        if location.as_raw().is_null() {
            return Ok(None);
        }

        let lat = env.call_method(&location, "getLatitude", "()D", &[])?.d()?;
        let lng = env
            .call_method(&location, "getLongitude", "()D", &[])?
            .d()?;
        // `accuracy_m` is not an `Option` in `GpsFix`, so a fix without one needs a
        // value that cannot be mistaken for a good measurement. Infinity is the
        // honest reading of "unbounded uncertainty" and fails every `accuracy <
        // threshold` test; zero would claim a perfect fix. In practice gps, network
        // and fused all report accuracy, so this arm is a mock provider's.
        let accuracy_m = if env.call_method(&location, "hasAccuracy", "()Z", &[])?.z()? {
            f64::from(env.call_method(&location, "getAccuracy", "()F", &[])?.f()?)
        } else {
            f64::INFINITY
        };
        let speed_ms = if env.call_method(&location, "hasSpeed", "()Z", &[])?.z()? {
            Some(f64::from(
                env.call_method(&location, "getSpeed", "()F", &[])?.f()?,
            ))
        } else {
            None
        };
        // `getBearing` is the *direction of travel*, not a compass heading — which
        // is what `GpsFix::heading_deg` means and what driver mode wants (see the
        // course-over-ground note in `systems::driver`). Degrees clockwise from
        // true north in both, so no conversion.
        let heading_deg = if env.call_method(&location, "hasBearing", "()Z", &[])?.z()? {
            Some(f64::from(
                env.call_method(&location, "getBearing", "()F", &[])?.f()?,
            ))
        } else {
            None
        };
        let nanos = env
            .call_method(&location, "getElapsedRealtimeNanos", "()J", &[])?
            .j()?;

        Ok(Some(Reading {
            fix: GpsFix {
                lat,
                lng,
                accuracy_m,
                speed_ms,
                heading_deg,
            },
            nanos,
        }))
    })
}

// ---------------------------------------------------------------------------
// 4. The camera, for a saved place's photo
// ---------------------------------------------------------------------------

/// How often [`CameraCapture::poll`] is allowed to touch the framework. It runs
/// from a Bevy system, i.e. sixty times a second, and each poll is a `query` into
/// another process; once a second is far faster than a human can take a photo.
const CAMERA_POLL_INTERVAL: Duration = Duration::from_millis(750);

/// `Intent.FLAG_GRANT_READ_URI_PERMISSION` | `FLAG_GRANT_WRITE_URI_PERMISSION`.
const FLAG_GRANT_READ_WRITE_URI: i32 = 0x0000_0003;

/// Refuse to read a file larger than this. A camera frame is single-digit
/// megabytes; anything at this size is a video the user picked in a gallery that
/// answered the intent, and decoding it would be a kill by the low-memory killer
/// rather than an error.
const MAX_PHOTO_BYTES: i64 = 64 * 1024 * 1024;

/// Serial number for the file name, so two photos taken in the same second do not
/// collide in the MediaStore.
static NEXT_PHOTO: AtomicI32 = AtomicI32::new(0);

/// A camera capture in flight: the `MediaStore` row we asked the camera app to
/// write into, and enough state to tell "not written yet" from "written".
///
/// **This is the design the no-dex rule forces, and it is worth understanding
/// before changing it.** The ordinary way to take a photo is
/// `startActivityForResult(ACTION_IMAGE_CAPTURE)` and read the answer in
/// `onActivityResult` — but `android-activity` 0.6 surfaces no activity result,
/// and delivering one would mean a Java class, hence a `classes.dex`, hence
/// Gradle (see the module note). So the intent is started fire-and-forget with an
/// `EXTRA_OUTPUT` pointing at a row this app inserted, and the completion signal
/// is the row filling up. The camera app never has to tell us anything.
///
/// *Guaranteed by contract:* `ACTION_IMAGE_CAPTURE` with `EXTRA_OUTPUT` is
/// documented to write the full-size image to that URI, and a URI permission
/// granted on the intent reaches the app that handles it. *Needs a device:* every
/// timing question below — whether the size settles before the file is closed on a
/// given OEM's camera, and whether a cancelled capture really does leave the row
/// empty rather than deleting it.
pub struct CameraCapture {
    /// The `content://` row the camera app writes into.
    uri: GlobalRef,
    /// Set once a result has been handed out, so a `poll` after that is inert.
    done: bool,
    /// When the last framework query happened (see [`CAMERA_POLL_INTERVAL`]).
    last_poll: Option<std::time::Instant>,
    /// Byte count seen on the previous poll, and how many polls have agreed on it.
    ///
    /// The completion test is "non-zero and unchanged since the last look" rather
    /// than "non-zero", because a poll can land in the middle of the camera app's
    /// write. The cost of being wrong is not a corrupt file — `utils::photo`
    /// would reject a truncated JPEG — but an error message for a photo that was
    /// taken perfectly well, which is worse than waiting another 750 ms.
    last_size: i64,
    stable_polls: u8,
}

/// Ask the camera app for a photo.
///
/// `ACTION_IMAGE_CAPTURE` needs no permission from the app that sends it — but
/// an app that *declares* `CAMERA` must also hold it before the intent is
/// honoured, and this one declares it for calls. So `platform::photo` asks for
/// [`PERM_CAMERA`] before calling this.
pub fn start_camera_capture() -> Result<CameraCapture, String> {
    let java_vm = vm()?;
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;
    let activity = activity();
    match start_camera_capture_inner(&mut env, &activity) {
        Ok(uri) => {
            tracing::info!("camera intent started; waiting on the MediaStore row");
            Ok(CameraCapture {
                uri,
                done: false,
                last_poll: None,
                last_size: -1,
                stable_polls: 0,
            })
        }
        Err(Fail(msg)) => {
            clear_pending(&env);
            Err(msg)
        }
    }
}

fn start_camera_capture_inner(env: &mut JNIEnv, activity: &JObject) -> Result<GlobalRef, Fail> {
    let uri = insert_photo_row(env, activity)?;
    match launch_camera(env, activity, &uri) {
        Ok(()) => Ok(uri),
        Err(e) => {
            // The row was inserted before the camera app was known to exist, so a
            // device with no camera would otherwise leave an empty file in the
            // user's gallery for every attempt.
            clear_pending(env);
            delete_row(env, activity, &uri);
            Err(e)
        }
    }
}

/// Insert an empty image row and return its `content://` URI.
fn insert_photo_row(env: &mut JNIEnv, activity: &JObject) -> Result<GlobalRef, Fail> {
    env.with_local_frame_returning_local(16, |env| {
        let resolver = env
            .call_method(
                activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;
        // `MediaStore.Images.Media.EXTERNAL_CONTENT_URI`. The nested class spells
        // as `MediaStore$Images$Media` in JNI's binary-name form.
        let collection = env
            .get_static_field(
                "android/provider/MediaStore$Images$Media",
                "EXTERNAL_CONTENT_URI",
                "Landroid/net/Uri;",
            )?
            .l()?;

        let values = env.new_object("android/content/ContentValues", "()V", &[])?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let serial = NEXT_PHOTO.fetch_add(1, Ordering::Relaxed);
        put_string(
            env,
            &values,
            "_display_name",
            &format!("cartopolis-{stamp}-{serial}.jpg"),
        )?;
        put_string(env, &values, "mime_type", "image/jpeg")?;
        // `RELATIVE_PATH` arrived with scoped storage in API 29. Below that the
        // column does not exist and inserting it throws, so API 28 gets the
        // gallery's default location instead — which is also why the manifest
        // carries WRITE_EXTERNAL_STORAGE with `maxSdkVersion="28"`.
        if sdk_int(env)? >= 29 {
            put_string(env, &values, "relative_path", "Pictures/Cartopolis")?;
        }
        // Deliberately *not* setting `IS_PENDING`: it restricts the row to the
        // owning app, and the app that has to write here is the camera.

        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&collection), JValue::Object(&values)],
            )?
            .l()?;
        if uri.is_null() {
            return Err(Fail(
                "the MediaStore refused a new image row (no storage permission?)".to_string(),
            ));
        }
        Ok(uri)
    })
    .and_then(|uri| Ok(env.new_global_ref(&uri)?))
}

/// `ContentValues.put(String, String)`.
fn put_string(env: &mut JNIEnv, values: &JObject, key: &str, value: &str) -> Result<(), Fail> {
    let key: JObject = env.new_string(key)?.into();
    let value: JObject = env.new_string(value)?.into();
    env.call_method(
        values,
        "put",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[JValue::Object(&key), JValue::Object(&value)],
    )?;
    Ok(())
}

/// `Build.VERSION.SDK_INT`.
fn sdk_int(env: &mut JNIEnv) -> Result<i32, Fail> {
    Ok(env
        .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
        .i()?)
}

/// Start `ACTION_IMAGE_CAPTURE` pointed at `uri`.
///
/// Started on the **activity**, not the application context, and without
/// `FLAG_ACTIVITY_NEW_TASK` — unlike [`open_url`], which hands a link to another
/// app and does not care where it lands. A camera launched into our own task
/// returns here when the user is done, which is the behaviour that makes the
/// photo appear in the panel they left.
fn launch_camera(env: &mut JNIEnv, activity: &JObject, uri: &GlobalRef) -> Result<(), Fail> {
    env.with_local_frame(12, |env| {
        let action: JObject = env.new_string("android.media.action.IMAGE_CAPTURE")?.into();
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action)],
        )?;
        // `MediaStore.EXTRA_OUTPUT` is the literal "output"; a Uri is Parcelable,
        // so this is the Parcelable overload rather than the String one.
        let extra: JObject = env.new_string("output")?.into();
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Landroid/os/Parcelable;)Landroid/content/Intent;",
            &[JValue::Object(&extra), JValue::Object(uri.as_obj())],
        )?;
        // Without the grant the camera app cannot open our row for writing, and
        // the capture silently produces nothing at all.
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_GRANT_READ_WRITE_URI)],
        )?;
        // Throws ActivityNotFoundException on a device with no camera app, which
        // the caller turns into a message rather than a crash.
        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
}

impl CameraCapture {
    /// Look at the row. `None` while it is still empty (or too soon since the last
    /// look), `Some` exactly once when the photo has landed or the read failed.
    pub fn poll(&mut self) -> Option<Result<Vec<u8>, String>> {
        if self.done {
            return None;
        }
        let now = std::time::Instant::now();
        if let Some(last) = self.last_poll {
            if now.duration_since(last) < CAMERA_POLL_INTERVAL {
                return None;
            }
        }
        self.last_poll = Some(now);

        let java_vm = match vm() {
            Ok(vm) => vm,
            Err(e) => {
                self.done = true;
                return Some(Err(e));
            }
        };
        let mut env = match java_vm.attach_current_thread() {
            Ok(env) => env,
            Err(e) => {
                self.done = true;
                return Some(Err(format!("could not attach to the JVM: {e}")));
            }
        };
        let activity = activity();

        let size = match row_size(&mut env, &activity, &self.uri) {
            Ok(size) => size,
            Err(Fail(msg)) => {
                clear_pending(&env);
                self.done = true;
                return Some(Err(msg));
            }
        };
        if size <= 0 {
            self.stable_polls = 0;
            self.last_size = size;
            return None;
        }
        if size > MAX_PHOTO_BYTES {
            self.done = true;
            return Some(Err("that photo is too large to attach".to_string()));
        }
        if size != self.last_size {
            self.last_size = size;
            self.stable_polls = 0;
            return None;
        }
        self.stable_polls = self.stable_polls.saturating_add(1);
        if self.stable_polls < 1 {
            return None;
        }

        self.done = true;
        match read_uri(&mut env, &activity, &self.uri) {
            Ok(bytes) => Some(Ok(bytes)),
            Err(Fail(msg)) => {
                clear_pending(&env);
                Some(Err(msg))
            }
        }
    }

    /// Drop the row the camera app was given.
    ///
    /// Only for a capture the user abandoned. A *successful* one deliberately
    /// leaves the row in place: the photo is in their gallery, where a photo they
    /// took with their camera belongs, and deleting it behind their back would be
    /// the surprising choice. That is why this is an explicit call and not a
    /// `Drop` impl.
    pub fn discard(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        let Ok(java_vm) = vm() else { return };
        let Ok(env) = java_vm.attach_current_thread() else {
            return;
        };
        let mut env = env;
        let activity = activity();
        delete_row(&mut env, &activity, &self.uri);
    }
}

/// The `_size` column of the row, or 0 when it is null (the state a freshly
/// inserted row is in until the camera app writes).
fn row_size(env: &mut JNIEnv, activity: &JObject, uri: &GlobalRef) -> Result<i64, Fail> {
    env.with_local_frame(16, |env| {
        let resolver = env
            .call_method(
                activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;
        let null = JObject::null();
        let projection = env.new_object_array(1, "java/lang/String", &null)?;
        let column: JObject = env.new_string("_size")?.into();
        env.set_object_array_element(&projection, 0, &column)?;
        let projection: JObject = projection.into();
        let cursor = env
            .call_method(
                &resolver,
                "query",
                "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;\
                  [Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
                &[
                    JValue::Object(uri.as_obj()),
                    JValue::Object(&projection),
                    JValue::Object(&null),
                    JValue::Object(&null),
                    JValue::Object(&null),
                ],
            )?
            .l()?;
        if cursor.is_null() {
            // The row is gone — the user deleted it from the gallery, or the
            // provider dropped it. Nothing to wait for any more.
            return Err(Fail("the photo was removed before it arrived".to_string()));
        }
        let size = if env.call_method(&cursor, "moveToFirst", "()Z", &[])?.z()? {
            if env
                .call_method(&cursor, "isNull", "(I)Z", &[JValue::Int(0)])?
                .z()?
            {
                0
            } else {
                env.call_method(&cursor, "getLong", "(I)J", &[JValue::Int(0)])?
                    .j()?
            }
        } else {
            0
        };
        // A Cursor holds a binder-side window; leaving it to the finalizer would
        // log a warning on every poll.
        env.call_method(&cursor, "close", "()V", &[])?;
        Ok(size)
    })
}

/// Read the whole row through `ContentResolver.openInputStream`.
fn read_uri(env: &mut JNIEnv, activity: &JObject, uri: &GlobalRef) -> Result<Vec<u8>, Fail> {
    env.with_local_frame(16, |env| {
        let resolver = env
            .call_method(
                activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;
        let stream = env
            .call_method(
                &resolver,
                "openInputStream",
                "(Landroid/net/Uri;)Ljava/io/InputStream;",
                &[JValue::Object(uri.as_obj())],
            )?
            .l()?;
        if stream.is_null() {
            return Err(Fail("the photo could not be opened".to_string()));
        }

        const CHUNK: i32 = 64 * 1024;
        let buffer = env.new_byte_array(CHUNK)?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let read = env
                .call_method(
                    &stream,
                    "read",
                    "([BII)I",
                    &[JValue::Object(&buffer), JValue::Int(0), JValue::Int(CHUNK)],
                )?
                .i()?;
            if read <= 0 {
                break;
            }
            let mut chunk = vec![0i8; read as usize];
            env.get_byte_array_region(&buffer, 0, &mut chunk)?;
            // Java bytes are signed; the reinterpretation is exact.
            out.extend(chunk.into_iter().map(|b| b as u8));
            if out.len() as i64 > MAX_PHOTO_BYTES {
                return Err(Fail("that photo is too large to attach".to_string()));
            }
        }
        env.call_method(&stream, "close", "()V", &[])?;
        Ok(out)
    })
}

/// `ContentResolver.delete(uri, null, null)`, best-effort.
fn delete_row(env: &mut JNIEnv, activity: &JObject, uri: &GlobalRef) {
    let result: Result<(), Fail> = env.with_local_frame(8, |env| {
        let resolver = env
            .call_method(
                activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;
        let null = JObject::null();
        env.call_method(
            &resolver,
            "delete",
            "(Landroid/net/Uri;Ljava/lang/String;[Ljava/lang/String;)I",
            &[
                JValue::Object(uri.as_obj()),
                JValue::Object(&null),
                JValue::Object(&null),
            ],
        )?;
        Ok(())
    });
    if result.is_err() {
        clear_pending(env);
    }
}

/// This device's UnifiedPush endpoint, if a distributor has given it one
/// (protocol 29, `systems::push`). The Java side — `Push.java` registers at
/// startup, `PushReceiver.java` keeps the answer — writes it to the app's
/// `cartopolis_push` preferences, and this reads it back: the endpoint arrives
/// on a broadcast at any moment, and a preferences file is the one place both
/// a receiver with no native code loaded and the running app can see.
pub fn push_endpoint() -> Option<String> {
    let java_vm = vm().ok()?;
    let mut env = java_vm.attach_current_thread().ok()?;
    let activity = activity();
    let result = env.with_local_frame(8, |env| {
        let name = env.new_string("cartopolis_push")?;
        let prefs = env
            .call_method(
                &activity,
                "getSharedPreferences",
                "(Ljava/lang/String;I)Landroid/content/SharedPreferences;",
                &[JValue::Object(&name), JValue::Int(0)],
            )?
            .l()?;
        let key = env.new_string("endpoint")?;
        let value = env
            .call_method(
                &prefs,
                "getString",
                "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&key), JValue::Object(&JObject::null())],
            )?
            .l()?;
        if value.is_null() {
            return Ok::<Option<String>, Fail>(None);
        }
        Ok(Some(env.get_string(&JString::from(value))?.into()))
    });
    match result {
        Ok(v) => v.filter(|e| !e.trim().is_empty()),
        Err(Fail(msg)) => {
            clear_pending(&env);
            tracing::warn!(error = %msg, "could not read the push endpoint");
            None
        }
    }
}

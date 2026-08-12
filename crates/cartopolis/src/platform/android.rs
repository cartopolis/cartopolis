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
//! - **Keep-screen-on** ([`set_keep_screen_on`]) turned out to need no JNI at all:
//!   the NDK already carries the UI-thread hop we would otherwise have needed a
//!   `Runnable` for.
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
use std::sync::Arc;
use std::time::Duration;

use bevy::android::android_activity::WindowManagerFlags;
use bevy::android::ANDROID_APP;
use jni::objects::{GlobalRef, JObject, JString, JValue};
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
// 2. Keep the screen on
// ---------------------------------------------------------------------------

/// Hold the screen awake while the app is in the foreground (driver mode's reason
/// for existing: the phone otherwise blanks thirty seconds into a drive).
///
/// **This needs no JNI and no `Runnable`, and that is the interesting part.** The
/// obvious route — `Window.addFlags(FLAG_KEEP_SCREEN_ON)` — has to run on the Java
/// UI thread, and `Activity.runOnUiThread` takes a `Runnable`, i.e. a class we
/// cannot define. But the NDK already exposes exactly this one operation with the
/// hop built in: `ANativeActivity_setWindowFlags`, which android-activity wraps as
/// `AndroidApp::set_window_flags`, writes a `CMD_SET_WINDOW_FLAGS` work item into
/// the activity's main-work pipe; the **UI thread's** looper reads it and performs
/// the `getWindow().setFlags()` itself. So it is safe to call from any thread by
/// construction, not by luck.
///
/// *Guaranteed by contract:* the NDK documents `ANativeActivity_setWindowFlags` as
/// "calls `getWindow().setFlags()` of the given activity", and the pipe hop is
/// AOSP's implementation of it. *Needs a device:* that the flag survives the
/// activity's own window recreation on a configuration change — the manifest's
/// `configChanges` list is meant to prevent that recreation, so the expectation is
/// that it does, but nothing here re-asserts the flag if it doesn't.
// `allow`: `platform` is a *private* module, so a `pub fn` nothing in the crate
// calls is dead code — and this deliberately has no caller yet (driver mode is
// where it belongs). Delete the attribute with the first call site, not before:
// its absence is what will report the *next* orphan here.
#[allow(dead_code)]
pub fn set_keep_screen_on(on: bool) {
    let Some(app) = ANDROID_APP.get() else {
        return;
    };
    let (add, remove) = if on {
        (
            WindowManagerFlags::KEEP_SCREEN_ON,
            WindowManagerFlags::empty(),
        )
    } else {
        (
            WindowManagerFlags::empty(),
            WindowManagerFlags::KEEP_SCREEN_ON,
        )
    };
    app.set_window_flags(add, remove);
    tracing::info!(on, "keep-screen-on");
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
// `allow`: see `set_keep_screen_on`. This one is expected to stay uncalled in
// shipped code — wire it to a debug affordance for the one run, then delete it.
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
// `allow`: see `set_keep_screen_on`. `data_layers::open_url`'s catch-all arm is the
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

/// The number of trailing log lines [`own_logcat`] asks for.
///
/// Sized against the *consumer*, not the producer: the result is carried to the
/// share sheet in an `Intent` extra, and an intent crosses a binder transaction
/// with a hard limit around a megabyte. 600 lines of logcat is well inside that
/// and comfortably spans a launch that dies before its first frame.
const LOGCAT_TAIL_LINES: &str = "600";

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
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
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
/// `PendingIntent.FLAG_IMMUTABLE`. Mandatory on API 31+ (creating a `PendingIntent`
/// with neither mutability flag throws `IllegalArgumentException` there), and
/// harmless below it.
const PENDING_INTENT_FLAG_IMMUTABLE: i32 = 0x0400_0000;

/// Providers we will use, best first.
///
/// `fused` is the **platform's** `LocationManager.FUSED_PROVIDER`, not Google Play
/// Services' `FusedLocationProviderClient` — the latter arrives as an AAR and is
/// therefore a Gradle project, i.e. out of reach for this app by construction. It
/// is only public API from API 31, so rather than gate on the SDK level we take
/// whatever the platform itself lists as enabled (see [`enabled_providers`]) and
/// intersect: a name the framework just told us about cannot be one it rejects.
const PROVIDERS: [&str; 3] = ["fused", "gps", "network"];

/// How often the cached fix is re-read. GNSS reports at ~1 Hz; polling faster buys
/// nothing but JNI calls.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// How long we keep asking `checkSelfPermission` after putting the dialog up.
const PERMISSION_WAIT: Duration = Duration::from_secs(120);
/// The **first** fix we publish must be at most this old. A cached position from
/// this morning is not "where the device is"; it is where it was parked, and an
/// arrow placed there is worse than an arrow that has not appeared yet. Later fixes
/// need only be newer than the last published one.
const FIRST_FIX_MAX_AGE: Duration = Duration::from_secs(60);
/// Requested update period, milliseconds. This is a *hint* to the provider and the
/// framework is free to be slower.
const UPDATE_INTERVAL_MS: i64 = 1000;

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

/// The outcome of asking for the location permission.
enum Permission {
    Granted,
    Denied,
    /// The watch was dropped while the dialog was up.
    Cancelled,
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
/// - **An immutable `PendingIntent` keeps the registration alive.** Google's own
///   guidance says location updates are a case for `FLAG_MUTABLE`, and the reported
///   consequence of getting it wrong is precise and reassuring: with
///   `FLAG_IMMUTABLE` the `Location` extra "returns as null in your receiver class"
///   — the broadcast is still *sent*, it simply carries nothing. That is the whole
///   cost, and we have no receiver to be disappointed. `FLAG_IMMUTABLE` is
///   therefore both the security-correct choice for an intent we never consume and
///   the one whose documented failure mode we are already living with. The residual
///   risk is that some OEM's `LocationManager` rejects an immutable intent outright
///   rather than sending an empty one; if a device logs the registration going away
///   after the first fix, this constant is the first thing to flip.
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
    let java_vm = vm()?;
    // Attached for the thread's whole life and detached by dropping this guard on
    // the way out. Per-call attach/detach would be the dominant cost of a 1 Hz
    // poll, and a permanent attach would leave the thread attached after it exits.
    let mut env = java_vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach the location thread to the JVM: {e}"))?;
    let activity = activity();

    match settle_permission(&mut env, &activity, cancel) {
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

    // Request updates on the best one only. Every extra registration is another
    // radio kept warm for a fix we are already getting; `fused` in particular
    // already blends GNSS, wifi and cell.
    let request_code = NEXT_REQUEST_CODE.fetch_add(1, Ordering::Relaxed);
    let pending = match request_updates(&mut env, &activity, &manager, usable[0], request_code) {
        Ok(pi) => Some(pi),
        Err(Fail(msg)) => {
            // Not fatal. Without it we are reduced to whatever fix some other app
            // on the phone happens to produce — which is a genuinely worse but
            // still working provider, and much better than no driver mode at all.
            clear_pending(&env);
            tracing::warn!(error = %msg, "requestLocationUpdates failed — falling back to polling the cached fix only");
            None
        }
    };

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
                last_nanos = reading.nanos;
                published = true;
                report(slot, Ok(reading.fix));
            }
        }

        if !nap(cancel, POLL_INTERVAL) {
            break;
        }
    }

    if let Some(pending) = pending {
        // Releasing the radio is the whole reason a watch is a subscription rather
        // than a global. Best-effort: the thread is on its way out and there is
        // nobody left to report to.
        if let Err(Fail(msg)) = remove_updates(&mut env, &manager, &pending) {
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

fn settle_permission(
    env: &mut JNIEnv,
    activity: &JObject,
    cancel: &AtomicBool,
) -> Result<Permission, Fail> {
    if permission_granted(env, activity)? {
        return Ok(Permission::Granted);
    }
    request_permission(env, activity)?;
    // **Polling, because the answer is unreachable.** The result arrives at
    // `Activity.onRequestPermissionsResult`, which is a Java method we would have
    // to override — a class, and therefore out. `Context.checkSelfPermission` is a
    // plain framework call that reads the current grant, so asking it repeatedly
    // gets the same information a callback would, a beat later. The user may sit on
    // the dialog, or wander into Settings, hence two minutes rather than seconds.
    let steps = PERMISSION_WAIT.as_millis() / 500;
    for _ in 0..steps {
        if !nap(cancel, Duration::from_millis(500)) {
            return Ok(Permission::Cancelled);
        }
        if permission_granted(env, activity)? {
            return Ok(Permission::Granted);
        }
    }
    Ok(Permission::Denied)
}

/// Whether we hold *either* location permission.
///
/// Coarse alone is accepted rather than treated as a denial: it yields a real fix
/// with an honest `accuracy_m` of a kilometre or two, which the caller can judge,
/// and refusing it would turn "the user picked Approximate" into "location is
/// broken".
fn permission_granted(env: &mut JNIEnv, activity: &JObject) -> Result<bool, Fail> {
    env.with_local_frame(8, |env| {
        for name in [PERM_FINE, PERM_COARSE] {
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
fn request_permission(env: &mut JNIEnv, activity: &JObject) -> Result<(), Fail> {
    env.with_local_frame(8, |env| {
        let null = JObject::null();
        let names = env.new_object_array(2, "java/lang/String", &null)?;
        let fine: JObject = env.new_string(PERM_FINE)?.into();
        let coarse: JObject = env.new_string(PERM_COARSE)?.into();
        env.set_object_array_element(&names, 0, &fine)?;
        env.set_object_array_element(&names, 1, &coarse)?;
        let names: JObject = names.into();
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
                    JValue::Int(PENDING_INTENT_FLAG_IMMUTABLE),
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

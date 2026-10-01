// The app's activity: `android.app.NativeActivity` plus one rule — the process
// ends with the activity.
//
// ## Why this class exists
//
// `android_main` (crates/cartopolis_android/src/lib.rs) ends the process when
// `cartopolis::run()` returns, on the belief that run() returns when the activity
// is destroyed. It does not. winit's Android backend receives
// `MainEvent::Destroy` and answers with `warn!("TODO: forward onDestroy
// notification to application")`, so the event loop keeps polling a glue instance
// whose activity is gone. The only way run() ever returned was the app's own back
// gesture writing `AppExit`.
//
// Everywhere else the activity dies — swiped out of recents, destroyed by the
// system for memory, "Don't keep activities" — the process outlived it (kept warm
// by the background-connection service, or simply cached). The next tap on the
// icon then created a new activity in that process, started a second
// `android_main` thread, hit its re-entry guard and exited: the app opened and
// closed itself, and only the tap after that started cleanly.
//
// The activity is the one place that is told about its own destruction, so it is
// the place to act on it. Killing rather than `System.exit`: no shutdown hooks
// and no static destructors, which run on whichever thread called them while the
// render and tokio threads are still live. The user's data is already written —
// every save in this app is eager, and the autosave has flushed on suspend.

package eu.garage44.cartopolis;

import android.app.NativeActivity;
import android.app.NotificationManager;
import android.os.Bundle;
import android.os.Process;

public final class MainActivity extends NativeActivity {
    /** Whether the app is on screen, for `PushReceiver`: a wake-up for a call
     *  the app is already showing must not ring a second time. */
    static volatile boolean onScreen;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Where the simulator wakes this phone while the app is closed
        // (Push.java). Nothing happens without a UnifiedPush distributor.
        Push.register(this);
    }

    @Override
    protected void onResume() {
        super.onResume();
        onScreen = true;
        // Opened, from the notification or not: the app shows the call now.
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager != null) {
            manager.cancel(PushReceiver.NOTIFICATION_ID);
        }
    }

    @Override
    protected void onPause() {
        onScreen = false;
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        // **Before** `super`, never after it. `NativeActivity.onDestroy` calls into
        // android-activity's glue (`native_activity/glue.rs`, `on_destroy`), which
        // posts `Destroy` to the `android_main` thread and then blocks *this*, the
        // UI thread, until that thread has returned. winit never forwards
        // `Destroy`, so `android_main` never returns and `super.onDestroy()` never
        // comes back: a kill placed after it was unreachable. The process stayed
        // up with its main thread wedged — held there by the background
        // connection's foreground service — and every later launch went into that
        // process and got as far as the theme's splash, for good. That is what a
        // swipe from recents, a tap on the notification and "the app does not
        // reopen" all were.
        //
        // Whatever the reason for the destruction, this process has no way to host
        // another activity (`bevy::android::ANDROID_APP` is set once per process),
        // so leaving is what lets the next launch start clean.
        Process.killProcess(Process.myPid());
        super.onDestroy();
    }
}

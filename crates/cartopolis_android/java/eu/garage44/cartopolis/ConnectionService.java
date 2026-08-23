// The foreground service that keeps the process alive while the app is not on
// screen, so the connection in `network.rs`'s thread survives being backgrounded.
//
// ## Why a service exists at all
//
// Android freezes a cached process. Threads are not the issue — the wire already
// runs on its own OS thread with its own tokio runtime, entirely outside Bevy's
// schedule, so nothing about the connection needs the activity. What needs the
// activity is *being allowed to keep running*, and the only thing that buys that
// without a push service is a foreground service with an ongoing notification.
//
// The alternative was Firebase Cloud Messaging, rejected because it is Google
// Play services only. That is a project constraint, not a technical one, and it
// is why this file is 90 lines of AOSP instead of a dependency.
//
// ## Why this class holds no logic
//
// It starts nothing and connects to nothing. The socket, the reconnect and every
// byte on it live in Rust and were already running before this service started;
// killing the service does not close the connection, it only removes the
// permission to stay alive. So the entire job here is `startForeground` — the
// notification *is* the feature. Resist adding state: two owners of one
// connection is the bug this shape avoids.

package eu.garage44.cartopolis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

public final class ConnectionService extends Service {
    /** Stable across restarts: the channel is created once and reused. */
    private static final String CHANNEL_ID = "connection";
    /** Any non-zero id; startForeground rejects 0. */
    private static final int NOTIFICATION_ID = 1;

    /** Not a bound service — Rust starts it with an Intent and nothing binds. */
    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        ensureChannel();
        // The three-argument overload arrived in API 29 and is *required* from
        // API 34, where a service whose declared type is not passed here throws
        // MissingForegroundServiceTypeException. minSdk is 28, so both paths ship.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, buildNotification(),
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } else {
            startForeground(NOTIFICATION_ID, buildNotification());
        }
        // START_NOT_STICKY: if the system kills this, do not bring it back. A
        // service restarted on its own would hold a notification for a process
        // whose connection died with it — an indicator for nothing.
        return START_NOT_STICKY;
    }

    /** Idempotent: creating an existing channel updates it and does not throw. */
    private void ensureChannel() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                "Background connection",
                // LOW, so the ongoing notification never makes a sound or peeks.
                // It is a status indicator, and one that pinged would be a reason
                // to turn the feature off.
                NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Shown while Cartopolis keeps its connection open in the background.");
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);
    }

    private Notification buildNotification() {
        // Tapping the notification returns to the running task rather than
        // starting a second one — the launch intent carries the launcher's
        // category, and the manifest's launchMode="singleTask" does the rest.
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        PendingIntent tap = null;
        if (launch != null) {
            // FLAG_IMMUTABLE is mandatory from API 31 for a PendingIntent an app
            // does not need to have filled in. (The location provider's is
            // deliberately mutable; this one has nothing to fill in.)
            tap = PendingIntent.getActivity(this, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        }
        Notification.Builder builder = new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("Cartopolis is connected")
                .setContentText("Staying online in the background.")
                .setSmallIcon(android.R.drawable.stat_sys_download_done)
                .setOngoing(true)
                .setShowWhen(false);
        if (tap != null) {
            builder.setContentIntent(tap);
        }
        return builder.build();
    }
}

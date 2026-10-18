// The foreground service a shared screen needs (`ScreenCapture.java`).
//
// Since Android 14 a MediaProjection may only be made while a foreground
// service of type `mediaProjection` runs, and that service's notification is
// the person's standing reminder that the screen is going out. It starts when
// the person said yes in the system dialog and stops with the sharing.

package eu.garage44.cartopolis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

public final class ScreenService extends Service {
    private static final String CHANNEL_ID = "screen";
    /** Not ConnectionService's id: both may be up at once. */
    private static final int NOTIFICATION_ID = 2;

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager != null) {
            NotificationChannel channel = new NotificationChannel(
                    CHANNEL_ID, "Screen sharing", NotificationManager.IMPORTANCE_LOW);
            channel.setDescription("Shown while your screen is shared in a call.");
            channel.setShowBadge(false);
            manager.createNotificationChannel(channel);
        }
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        Notification.Builder builder = new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("You are sharing your screen")
                .setContentText("Everybody in the call sees it. Stop it in Cartopolis.")
                .setSmallIcon(android.R.drawable.ic_menu_share)
                .setOngoing(true)
                .setShowWhen(false);
        if (launch != null) {
            builder.setContentIntent(PendingIntent.getActivity(this, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, builder.build(),
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
        } else {
            startForeground(NOTIFICATION_ID, builder.build());
        }
        if (intent != null) {
            Intent data = intent.getParcelableExtra("data");
            int code = intent.getIntExtra("code", 0);
            if (data != null) {
                ScreenCapture.begin(this, code, data);
            }
        }
        return START_NOT_STICKY;
    }

    static void stop(Context context) {
        context.stopService(new Intent(context, ScreenService.class));
    }
}

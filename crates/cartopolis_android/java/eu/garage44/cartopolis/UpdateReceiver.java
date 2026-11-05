// What the package installer says about a self-update (Updater.java).
//
// Success needs no answer: the installer ends this process to replace it, and
// the next launch is the new build. The one answer that needs us is
// STATUS_PENDING_USER_ACTION — Android wants the person to confirm, which it
// always does the first time (and every time below API 31). Its confirm screen
// is an activity, and an app may only start one while it is on screen; off
// screen it becomes a notification whose tap opens it.

package eu.garage44.cartopolis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInstaller;
import android.util.Log;

public final class UpdateReceiver extends BroadcastReceiver {
    private static final String CHANNEL_ID = "updates";
    /** Distinct from ConnectionService's 1 and PushReceiver's 2. */
    static final int NOTIFICATION_ID = 3;

    @Override
    public void onReceive(Context context, Intent intent) {
        int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (status != PackageInstaller.STATUS_PENDING_USER_ACTION) {
            if (status != PackageInstaller.STATUS_SUCCESS) {
                Log.w("cartopolis", "update: not installed (" + status + "): "
                        + intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE));
            }
            return;
        }
        Intent confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT);
        if (confirm == null) {
            return;
        }
        confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        if (MainActivity.onScreen) {
            context.startActivity(confirm);
            return;
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Updates", NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("A new version of Cartopolis is ready to install.");
        manager.createNotificationChannel(channel);
        manager.notify(NOTIFICATION_ID, new Notification.Builder(context, CHANNEL_ID)
                .setContentTitle("Cartopolis update ready")
                .setContentText("Tap to install the new version.")
                .setSmallIcon(android.R.drawable.stat_sys_download_done)
                .setAutoCancel(true)
                .setContentIntent(PendingIntent.getActivity(context, 0, confirm,
                        PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE))
                .build());
    }
}

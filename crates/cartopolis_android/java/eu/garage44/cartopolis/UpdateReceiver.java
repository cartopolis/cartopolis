// What the package installer says about a self-update (Updater.java), and the
// first word of the build that replaced us.
//
// Success needs no answer: the installer ends this process to replace it. The
// answer that needs us is STATUS_PENDING_USER_ACTION — Android wants the person
// to confirm, which it always does the first time (and every time below API
// 31). Its confirm screen is an activity, and an app may only start one while
// it is on screen; off screen it becomes a notification whose tap opens it.
//
// MY_PACKAGE_REPLACED arrives in the *new* build. Android does not reopen an app
// it has just updated, and an app may not open itself from the background, so
// when the person had tapped for the update a notification offers the way back.

package eu.garage44.cartopolis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageInstaller;
import android.util.Log;

public final class UpdateReceiver extends BroadcastReceiver {
    private static final String CHANNEL_ID = "updates";
    /** Distinct from ConnectionService's 1 and PushReceiver's 2. */
    static final int NOTIFICATION_ID = 3;

    @Override
    public void onReceive(Context context, Intent intent) {
        if (Intent.ACTION_MY_PACKAGE_REPLACED.equals(intent.getAction())) {
            SharedPreferences prefs = Updater.prefs(context);
            if (prefs.getBoolean(Updater.KEY_ASKED, false)) {
                prefs.edit().remove(Updater.KEY_ASKED).apply();
                Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
                if (launch != null) {
                    notify(context, "Cartopolis is updated", "Tap to open it.", launch);
                }
            }
            return;
        }
        int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (status == PackageInstaller.STATUS_SUCCESS) {
            return;
        }
        if (status != PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Log.w("cartopolis", "update: not installed (" + status + "): "
                    + intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE));
            Updater.notInstalled();
            return;
        }
        Intent confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT);
        if (confirm == null) {
            Updater.notInstalled();
            return;
        }
        confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        if (MainActivity.onScreen) {
            context.startActivity(confirm);
            return;
        }
        notify(context, "Cartopolis update ready", "Tap to install the new version.", confirm);
    }

    private static void notify(Context context, String title, String text, Intent tap) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Updates", NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("A new version of Cartopolis, ready or installed.");
        manager.createNotificationChannel(channel);
        manager.notify(NOTIFICATION_ID, new Notification.Builder(context, CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(text)
                .setSmallIcon(android.R.drawable.stat_sys_download_done)
                .setAutoCancel(true)
                .setContentIntent(PendingIntent.getActivity(context, 0, tap,
                        PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE))
                .build());
    }
}

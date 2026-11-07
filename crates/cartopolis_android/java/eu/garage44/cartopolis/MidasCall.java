// "A session wants the phone": the notification the Midas device link posts
// when a call reaches it while the app is off screen (`systems::midas_link`).
//
// The link stays open off screen, but nothing answers there: Bevy runs no
// update while the app is suspended. The person is usually on the Midas page
// on this same phone, so this says which session asks for what, peeking over
// that page, and one tap brings the app back to answer it. The hub holds the
// call for 90 s, so the notification goes after as long.

package eu.garage44.cartopolis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;

final class MidasCall {
    private static final String CHANNEL_ID = "midas";
    /** Distinct from ConnectionService's 1 and PushReceiver's 2. */
    static final int NOTIFICATION_ID = 3;
    /** How long the hub holds a call for a phone that is away. */
    private static final long HELD_MS = 90_000;

    private MidasCall() {}

    /** Show (or update) the notification; `waiting` counts the calls held. */
    static void show(Context context, String session, String tool, int waiting) {
        if (MainActivity.onScreen) {
            return;
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        // HIGH, so it peeks over the Midas page; silent, because the person
        // asked the session to work and is watching it do so.
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Midas sessions", NotificationManager.IMPORTANCE_HIGH);
        channel.setDescription("A Midas session wants the app while it is off screen.");
        channel.setSound(null, null);
        channel.enableVibration(false);
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);

        String text = waiting > 1
                ? tool + " and " + (waiting - 1) + " more. Tap to answer."
                : tool + ". Tap to answer.";
        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        Notification.Builder builder = new Notification.Builder(context, CHANNEL_ID)
                .setContentTitle(session + " wants the phone")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.stat_notify_more)
                .setCategory(Notification.CATEGORY_STATUS)
                .setAutoCancel(true)
                .setTimeoutAfter(HELD_MS);
        if (launch != null) {
            builder.setContentIntent(PendingIntent.getActivity(context, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        manager.notify(NOTIFICATION_ID, builder.build());
    }

    static void clear(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager != null) {
            manager.cancel(NOTIFICATION_ID);
        }
    }
}

// What the UnifiedPush distributor tells us (see Push.java): a new endpoint to
// keep, an endpoint taken away, or a message — which for this app only ever
// means "somebody started a call in one of your chats".
//
// ## Why the message is not read
//
// The simulator sends a wake-up with nothing in it (`simulator::push`): who is
// calling and from which chat never passes through the distributor's server.
// So this does not decode anything, it shows "Incoming call" and lets the app,
// once opened, show who — the roster reaches it over its own connection the
// moment it signs in. That also means this runs with no native code loaded,
// which is the situation it exists for: the app is closed.
//
// A broadcast without our token is not from our distributor and is ignored:
// the receiver is exported (it must be, the distributor is another app), so
// anyone could otherwise make the phone ring.

package eu.garage44.cartopolis;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.media.AudioAttributes;
import android.media.RingtoneManager;

public final class PushReceiver extends BroadcastReceiver {
    private static final String CHANNEL_ID = "calls";
    /** Distinct from ConnectionService's 1. */
    static final int NOTIFICATION_ID = 2;
    /** How long it rings before giving up: about as long as a call waits. */
    private static final long RING_MS = 45_000;

    @Override
    public void onReceive(Context context, Intent intent) {
        SharedPreferences prefs = Push.prefs(context);
        String token = intent.getStringExtra("token");
        if (token == null || !token.equals(prefs.getString(Push.KEY_TOKEN, null))) {
            return;
        }
        String action = intent.getAction();
        if (action == null) {
            return;
        }
        switch (action) {
            case "org.unifiedpush.android.connector.NEW_ENDPOINT":
                prefs.edit().putString(Push.KEY_ENDPOINT, intent.getStringExtra("endpoint")).apply();
                break;
            case "org.unifiedpush.android.connector.UNREGISTERED":
                prefs.edit().remove(Push.KEY_ENDPOINT).apply();
                break;
            case "org.unifiedpush.android.connector.MESSAGE":
                // On screen, the app is already ringing from the roster.
                if (!MainActivity.onScreen) {
                    ring(context);
                }
                break;
            default:
                break;
        }
    }

    private static void ring(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, "Calls", NotificationManager.IMPORTANCE_HIGH);
        channel.setDescription("Somebody is calling you in one of your chats.");
        channel.setSound(RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE),
                new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build());
        channel.enableVibration(true);
        manager.createNotificationChannel(channel);

        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        Notification.Builder builder = new Notification.Builder(context, CHANNEL_ID)
                .setContentTitle("Incoming call")
                .setContentText("Open Cartopolis to see who is calling.")
                .setSmallIcon(android.R.drawable.sym_call_incoming)
                .setCategory(Notification.CATEGORY_CALL)
                .setAutoCancel(true)
                .setTimeoutAfter(RING_MS);
        if (launch != null) {
            builder.setContentIntent(PendingIntent.getActivity(context, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        manager.notify(NOTIFICATION_ID, builder.build());
    }
}

// What the UnifiedPush distributor tells us (see Push.java): a new endpoint to
// keep, an endpoint taken away, or a message — which for this app means
// "somebody started a call in one of your chats" or "somebody wrote in one of
// them" (docs/notes/message-wake-ups.md).
//
// ## Why the message says so little
//
// The simulator sends a wake-up with one word in it (`simulator::push`):
// "message", "plan" (a new notice near a followed place), or anything else
// for a call. Who is calling or writing, and in
// which chat, never passes through the distributor's server. So this shows
// "Incoming call" or "New message" and lets the app, once opened, show who —
// the roster and the chats reach it over its own connection the moment it
// signs in. That also means this runs with no native code loaded,
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

import java.nio.charset.StandardCharsets;

public final class PushReceiver extends BroadcastReceiver {
    private static final String CHANNEL_ID = "calls";
    /** Distinct from ConnectionService's 1. */
    static final int NOTIFICATION_ID = 2;
    /** How long it rings before giving up: about as long as a call waits. */
    private static final long RING_MS = 45_000;
    private static final String MESSAGES_CHANNEL_ID = "messages";
    /** Distinct from the service's 1 and a call's 2. */
    static final int MESSAGE_NOTIFICATION_ID = 3;
    private static final String PLANS_CHANNEL_ID = "plans";
    /** Distinct from the service's 1, a call's 2 and a message's 3. */
    static final int PLAN_NOTIFICATION_ID = 4;

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
                // On screen, the app is already ringing from the roster, or
                // showing the chat.
                if (MainActivity.onScreen) {
                    break;
                }
                String word = word(intent);
                if ("message".equals(word)) {
                    tell(context);
                } else if ("plan".equals(word)) {
                    tellPlan(context);
                } else {
                    ring(context);
                }
                break;
            default:
                break;
        }
    }

    /** The push's word: "message" for a message, "plan" for a new notice near
     *  a followed place (docs/notes/plans.md). A call's is "1", and so is every
     *  push from a server older than message wake-ups. */
    private static String word(Intent intent) {
        byte[] bytes = intent.getByteArrayExtra("bytesMessage");
        return bytes != null
                ? new String(bytes, StandardCharsets.UTF_8)
                : intent.getStringExtra("message");
    }

    /** A notice was published near a place this account follows. Which one,
     *  and where, the app finds in the plans feed once opened. */
    private static void tellPlan(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                PLANS_CHANNEL_ID, "Plans near places you follow", NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("The municipality published something new near a place you follow.");
        manager.createNotificationChannel(channel);

        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        Notification.Builder builder = new Notification.Builder(context, PLANS_CHANNEL_ID)
                .setContentTitle("New plan near a place you follow")
                .setContentText("Open Cartopolis to see what was published.")
                .setSmallIcon(android.R.drawable.ic_dialog_map)
                .setCategory(Notification.CATEGORY_EVENT)
                .setOnlyAlertOnce(true)
                .setAutoCancel(true);
        if (launch != null) {
            builder.setContentIntent(PendingIntent.getActivity(context, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        manager.notify(PLAN_NOTIFICATION_ID, builder.build());
    }

    /** One notice for however many lines: a later push posts it again
     *  without a sound, until it is opened or swiped away. */
    private static void tell(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                MESSAGES_CHANNEL_ID, "Messages", NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("Somebody wrote to you in one of your chats while the app was closed.");
        manager.createNotificationChannel(channel);

        Intent launch = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        Notification.Builder builder = new Notification.Builder(context, MESSAGES_CHANNEL_ID)
                .setContentTitle("New message")
                .setContentText("Somebody wrote to you. Open Cartopolis to read it.")
                .setSmallIcon(android.R.drawable.sym_action_chat)
                .setCategory(Notification.CATEGORY_MESSAGE)
                .setOnlyAlertOnce(true)
                .setAutoCancel(true);
        if (launch != null) {
            builder.setContentIntent(PendingIntent.getActivity(context, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        manager.notify(MESSAGE_NOTIFICATION_ID, builder.build());
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

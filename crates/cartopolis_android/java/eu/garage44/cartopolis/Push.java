// UnifiedPush registration: where the simulator wakes this phone for a call
// while the app is closed (protocol 29, docs/notes/calls.md).
//
// UnifiedPush rather than Firebase for the reason ConnectionService gives: no
// Google Play services. The person installs a distributor app of their choice
// (ntfy is the common one); this asks it for an endpoint, `PushReceiver` keeps
// the answer in the `cartopolis_push` preferences, and Rust
// (`platform::android::push_endpoint`) reads it from there and tells the
// server. Without a distributor installed nothing happens and nothing is
// asked: the app works as it always has, ringing only while connected.
//
// The protocol is three broadcasts, spoken here by hand rather than through
// the connector library, which is an AAR — and an AAR is the Gradle this APK
// does not have (docs/notes/android-build.md).

package eu.garage44.cartopolis;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.os.Build;

import java.util.List;
import java.util.UUID;

final class Push {
    static final String PREFS = "cartopolis_push";
    static final String KEY_TOKEN = "token";
    static final String KEY_ENDPOINT = "endpoint";
    static final String KEY_DISTRIBUTOR = "distributor";

    private static final String REGISTER = "org.unifiedpush.android.distributor.REGISTER";

    private Push() {}

    static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    /**
     * Ask the installed distributor for an endpoint (again: a distributor
     * answers a repeat registration with the same endpoint, and it is how a
     * lost one comes back). The token is ours and random; a broadcast that
     * does not carry it is not from our distributor.
     */
    static void register(Activity activity) {
        SharedPreferences prefs = prefs(activity);
        List<ResolveInfo> found = activity.getPackageManager()
                .queryBroadcastReceivers(new Intent(REGISTER), 0);
        String chosen = null;
        String remembered = prefs.getString(KEY_DISTRIBUTOR, null);
        for (ResolveInfo r : found) {
            String pkg = r.activityInfo.packageName;
            if (pkg.equals(activity.getPackageName())) {
                continue;
            }
            if (chosen == null || pkg.equals(remembered)) {
                chosen = pkg;
            }
        }
        if (chosen == null) {
            return;
        }
        String token = prefs.getString(KEY_TOKEN, null);
        if (token == null) {
            token = UUID.randomUUID().toString();
        }
        prefs.edit().putString(KEY_TOKEN, token).putString(KEY_DISTRIBUTOR, chosen).apply();

        Intent intent = new Intent(REGISTER);
        intent.setPackage(chosen);
        intent.putExtra("token", token);
        intent.putExtra("application", activity.getPackageName());
        intent.putExtra("message", "Incoming calls");
        activity.sendBroadcast(intent);

        // Somebody who installed a distributor wants to be rung, and from API 33
        // a notification needs this permission. Asked only then — an app with
        // nothing to notify about has no reason to ask.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
                && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 44);
        }
    }
}

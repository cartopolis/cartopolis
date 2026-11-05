// The app updates itself from https://cartopolis.org/apk/, the one place it is
// published (docs/notes/android-build.md, "Publishing").
//
// ## What happens
//
// On every return to the screen, at most once in CHECK_EVERY_MS, a thread reads
// `version.json` beside the APK (`deploy/build/apk.sh` writes it). A higher
// version code than ours is downloaded into the cache — on any network, mobile
// data included, by the owner's choice (2026-11-05) — and checked twice before anything is installed: its
// sha256 against the published `cartopolis.apk.sha256`, and its package name and
// version code as Android itself reads them out of the archive. Then it waits for
// the person to leave the app, because installing an update ends the running
// process: the next time the app is opened it is the new build.
//
// ## When Android asks
//
// An app outside the Play Store cannot install anything silently the first time.
// The session below asks for USER_ACTION_NOT_REQUIRED (API 31+), which Android
// grants only once this app is the *installer of record* of itself — i.e. from
// the second self-update on; the first one, and every one on an older Android,
// comes back as STATUS_PENDING_USER_ACTION with the system's own confirm screen,
// which also carries the "Allow from this source" step the first time.
// `UpdateReceiver` shows it straight away if the app is on screen, and otherwise
// as a notification, since an activity cannot be started from the background.
//
// ## What is never updated
//
// A build whose version code is under PUBLISHED: a hand build (`build-apk.sh`
// falls back to the commit count) is debug-signed, so Android would refuse the
// published APK over it anyway, and it is somebody's work in progress.

package eu.garage44.cartopolis;

import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.os.Build;
import android.app.PendingIntent;
import android.util.Log;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.concurrent.atomic.AtomicBoolean;

final class Updater {
    private static final String TAG = "cartopolis";
    private static final String BASE = "https://cartopolis.org/apk/";
    /** `deploy/build/apk.sh`'s offset: every published build's code is above it. */
    private static final long PUBLISHED = 20000;
    private static final long CHECK_EVERY_MS = 6L * 60 * 60 * 1000;
    private static final String PREFS = "updater";
    private static final String KEY_CHECKED = "checked_at";

    /** One check, download or install at a time. */
    private static final AtomicBoolean busy = new AtomicBoolean();
    /** The verified download waiting for the person to leave the app. */
    private static volatile File ready;

    private Updater() {}

    /** From `MainActivity.onResume`: look for a newer build, now and then. */
    static void check(Context context) {
        Context app = context.getApplicationContext();
        long own = ownCode(app);
        if (own < PUBLISHED || ready != null) {
            return;
        }
        SharedPreferences prefs = app.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        long now = System.currentTimeMillis();
        if (now - prefs.getLong(KEY_CHECKED, 0) < CHECK_EVERY_MS) {
            return;
        }
        if (!busy.compareAndSet(false, true)) {
            return;
        }
        new Thread(() -> {
            try {
                File apk = fetch(app, own);
                // Checked, whatever came of it: a failed download is tried again
                // in CHECK_EVERY_MS, not on every return to the screen.
                prefs.edit().putLong(KEY_CHECKED, now).apply();
                if (apk != null) {
                    ready = apk;
                    Log.i(TAG, "update: " + apk.getName() + " is ready");
                }
            } catch (Exception e) {
                Log.w(TAG, "update: check failed", e);
            } finally {
                busy.set(false);
            }
            // Finished after the person left: nothing else will call it.
            if (ready != null && !MainActivity.onScreen) {
                install(app);
            }
        }, "updater").start();
    }

    /** From `MainActivity.onStop`: the app is off screen, so ending it is fine. */
    static void install(Context context) {
        File apk = ready;
        if (apk == null || !busy.compareAndSet(false, true)) {
            return;
        }
        Context app = context.getApplicationContext();
        new Thread(() -> {
            try {
                commit(app, apk);
                ready = null;
            } catch (Exception e) {
                Log.w(TAG, "update: install failed", e);
                ready = null;
                apk.delete();
            } finally {
                busy.set(false);
            }
        }, "updater").start();
    }

    /** The newer build, downloaded and verified, or null when there is none to
     *  take now. */
    private static File fetch(Context app, long own) throws IOException {
        File dir = new File(app.getCacheDir(), "update");
        dir.mkdirs();
        JSONObject version;
        try {
            version = new JSONObject(text(BASE + "version.json"));
        } catch (org.json.JSONException e) {
            throw new IOException("version.json: " + e.getMessage());
        }
        long code = version.optLong("code", 0);
        File apk = new File(dir, "cartopolis-" + code + ".apk");
        // Anything else in there is an older download, or one already installed.
        File[] old = dir.listFiles();
        if (old != null) {
            for (File f : old) {
                if (!f.equals(apk)) {
                    f.delete();
                }
            }
        }
        if (code <= own) {
            apk.delete();
            return null;
        }
        String want = text(BASE + "cartopolis.apk.sha256").trim().split("\\s+")[0];
        if (!(apk.isFile() && want.equals(sha256(apk)))) {
            download(BASE + "cartopolis.apk", apk);
            // Published between our two reads: the next check finds the pair whole.
            if (!want.equals(sha256(apk))) {
                apk.delete();
                throw new IOException("the APK does not match its published sha256");
            }
        }
        PackageInfo info = app.getPackageManager().getPackageArchiveInfo(apk.getPath(), 0);
        if (info == null || !app.getPackageName().equals(info.packageName)
                || info.getLongVersionCode() <= own) {
            apk.delete();
            throw new IOException("the APK is not a newer Cartopolis");
        }
        return apk;
    }

    private static void commit(Context app, File apk) throws IOException {
        PackageInstaller installer = app.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params =
                new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        params.setAppPackageName(app.getPackageName());
        params.setSize(apk.length());
        if (Build.VERSION.SDK_INT >= 31) {
            params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED);
        }
        int id = installer.createSession(params);
        try (PackageInstaller.Session session = installer.openSession(id)) {
            try (InputStream in = new FileInputStream(apk);
                 OutputStream out = session.openWrite("cartopolis.apk", 0, apk.length())) {
                copy(in, out);
                session.fsync(out);
            }
            // MUTABLE: the installer writes the outcome into this intent's extras.
            Intent result = new Intent(app, UpdateReceiver.class);
            PendingIntent status = PendingIntent.getBroadcast(app, id, result,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_MUTABLE);
            session.commit(status.getIntentSender());
            Log.i(TAG, "update: installing " + apk.getName());
        }
    }

    private static long ownCode(Context app) {
        try {
            return app.getPackageManager().getPackageInfo(app.getPackageName(), 0).getLongVersionCode();
        } catch (PackageManager.NameNotFoundException e) {
            return 0;
        }
    }

    private static HttpURLConnection open(String url) throws IOException {
        HttpURLConnection http = (HttpURLConnection) new URL(url).openConnection();
        http.setConnectTimeout(15_000);
        http.setReadTimeout(30_000);
        http.setUseCaches(false);
        if (http.getResponseCode() != 200) {
            http.disconnect();
            throw new IOException(url + ": HTTP " + http.getResponseCode());
        }
        return http;
    }

    private static String text(String url) throws IOException {
        HttpURLConnection http = open(url);
        try (InputStream in = http.getInputStream()) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            copy(in, out);
            return out.toString(StandardCharsets.UTF_8.name());
        } finally {
            http.disconnect();
        }
    }

    private static void download(String url, File to) throws IOException {
        File part = new File(to.getPath() + ".part");
        HttpURLConnection http = open(url);
        try (InputStream in = http.getInputStream(); OutputStream out = new FileOutputStream(part)) {
            copy(in, out);
        } finally {
            http.disconnect();
        }
        if (!part.renameTo(to)) {
            part.delete();
            throw new IOException("could not keep " + to.getName());
        }
    }

    private static String sha256(File file) throws IOException {
        MessageDigest digest;
        try {
            digest = MessageDigest.getInstance("SHA-256");
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IOException(e);
        }
        try (InputStream in = new FileInputStream(file)) {
            byte[] buf = new byte[1 << 16];
            for (int n; (n = in.read(buf)) > 0; ) {
                digest.update(buf, 0, n);
            }
        }
        StringBuilder hex = new StringBuilder();
        for (byte b : digest.digest()) {
            hex.append(String.format("%02x", b));
        }
        return hex.toString();
    }

    private static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buf = new byte[1 << 16];
        for (int n; (n = in.read(buf)) > 0; ) {
            out.write(buf, 0, n);
        }
    }
}

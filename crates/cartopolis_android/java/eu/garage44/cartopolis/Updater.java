// The app updates itself from https://cartopolis.org/apk/, the one place it is
// published (docs/notes/android-build.md, "The app updates itself").
//
// ## What happens
//
// On every return to the screen, at most once in CHECK_EVERY_MS, a thread reads
// `version.json` beside the APK (`deploy/build/apk.sh` writes it). A higher
// version code than ours is downloaded into the cache, on any network — mobile
// data included, by the owner's choice (2026-11-05). The app then shows "Update
// ready" (`platform::update`, `systems::app_update`), and installs it on the tap
// of Install (`installNow`) or else when the person leaves the app, because
// installing an update ends the running process.
//
// A server that refuses this build's wire version makes the app call
// `installNow` from its "Update now" card: the same path, without the wait.
//
// ## What is checked before anything is installed
//
// - **The signing certificate**, against our own. Android refuses an update
//   signed by anyone else on its own; checking first means a foreign file is
//   never put in front of the person as a confirm screen. Some Android versions
//   cannot read an archive's signers; then Android's own check is the one left.
// - Its sha256, against the published `cartopolis.apk.sha256`: the deploy
//   writes its files one by one, and a check mid-publish sees a pair that
//   disagrees and tries again later.
// - Its package name and version code, as Android reads them out of the
//   archive: never anything but a newer Cartopolis.
// - Its size, capped at MAX_BYTES while it downloads, so a wrong server cannot
//   fill the phone; and only https to cartopolis.org, redirects refused.
//
// **What this does not protect against:** the release key sits on the server
// that publishes the APK. Whoever takes that server over can sign an update
// every phone accepts. Accepted by the owner for now (2026-11-05).
//
// ## When Android asks
//
// An app outside the Play Store cannot install anything silently the first time.
// The session asks for USER_ACTION_NOT_REQUIRED (API 31+), which Android grants
// only once this app is the *installer of record* of itself — from the second
// self-update on; the first one, and every one on an older Android, comes back
// as STATUS_PENDING_USER_ACTION with the system's own confirm screen, which also
// carries the "Allow from this source" step the first time. `UpdateReceiver`
// shows it straight away if the app is on screen, and otherwise as a
// notification, since an activity cannot be started from the background.
//
// ## What is never updated
//
// A build whose version code is under PUBLISHED: a hand build (`build-apk.sh`
// falls back to the commit count) is debug-signed, so Android would refuse the
// published APK over it anyway, and it is somebody's work in progress.

package eu.garage44.cartopolis;

import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.content.pm.Signature;
import android.content.pm.SigningInfo;
import android.os.Build;
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
import java.util.Arrays;
import java.util.concurrent.atomic.AtomicBoolean;

final class Updater {
    private static final String TAG = "cartopolis";
    private static final String BASE = "https://cartopolis.org/apk/";
    /** `deploy/build/apk.sh`'s offset: every published build's code is above it. */
    private static final long PUBLISHED = 20000;
    private static final long CHECK_EVERY_MS = 6L * 60 * 60 * 1000;
    /** Three times today's APK. */
    private static final long MAX_BYTES = 300L * 1024 * 1024;
    private static final String PREFS = "updater";
    private static final String KEY_CHECKED = "checked_at";
    /** Set when the person asked for the install, so the new build says it is
     *  in (UpdateReceiver, on MY_PACKAGE_REPLACED). */
    static final String KEY_ASKED = "asked";

    /** One check, download or install at a time. */
    private static final AtomicBoolean busy = new AtomicBoolean();
    /** The verified download waiting for the person or for them to leave. */
    private static volatile File ready;
    private static volatile String readyName;
    /** Install as soon as the download is in: the person tapped for it. */
    private static volatile boolean wanted;
    /** What `status` says, for the app's own screen. */
    private static volatile String state;
    private static volatile long done;
    private static volatile long total;

    private Updater() {}

    /** From `MainActivity.onResume`: look for a newer build, now and then. */
    static void check(Context context) {
        Context app = context.getApplicationContext();
        if (ownCode(app) < PUBLISHED || ready != null) {
            return;
        }
        SharedPreferences prefs = prefs(app);
        if (System.currentTimeMillis() - prefs.getLong(KEY_CHECKED, 0) < CHECK_EVERY_MS) {
            return;
        }
        run(app);
    }

    /** The person tapped Install or Update now: fetch if need be, then install,
     *  on screen. */
    public static void installNow(Context context) {
        Context app = context.getApplicationContext();
        if (ownCode(app) < PUBLISHED) {
            return;
        }
        wanted = true;
        prefs(app).edit().putBoolean(KEY_ASKED, true).apply();
        if (ready != null) {
            install(app);
        } else {
            run(app);
        }
    }

    /** For `platform::update`: `ready <name>`, `fetching <done> <total>`,
     *  `installing`, `failed <why>`, or null for nothing to tell. */
    public static String status() {
        File apk = ready;
        if (apk != null && !"installing".equals(state)) {
            return "ready " + readyName;
        }
        String s = state;
        if ("fetching".equals(s)) {
            return "fetching " + done + " " + total;
        }
        return s;
    }

    /** From `MainActivity.onStop`: off screen, ending the process is fine. */
    static void onLeave(Context context) {
        install(context);
    }

    private static void run(Context app) {
        if (!busy.compareAndSet(false, true)) {
            return;
        }
        new Thread(() -> {
            long now = System.currentTimeMillis();
            try {
                File apk = fetch(app, ownCode(app));
                // Checked, whatever came of it: a failed download is tried again
                // in CHECK_EVERY_MS, not on every return to the screen.
                prefs(app).edit().putLong(KEY_CHECKED, now).apply();
                if (apk != null) {
                    ready = apk;
                    state = null;
                    Log.i(TAG, "update: " + apk.getName() + " is ready");
                } else if ("fetching".equals(state)) {
                    state = null;
                }
            } catch (Exception e) {
                Log.w(TAG, "update: check failed", e);
                state = "failed " + e.getMessage();
            } finally {
                busy.set(false);
            }
            // Asked for, or finished after the person left: nothing else will
            // call it.
            if (ready != null && (wanted || !MainActivity.onScreen)) {
                install(app);
            }
        }, "updater").start();
    }

    private static void install(Context context) {
        File apk = ready;
        if (apk == null || !busy.compareAndSet(false, true)) {
            return;
        }
        Context app = context.getApplicationContext();
        state = "installing";
        new Thread(() -> {
            try {
                commit(app, apk);
            } catch (Exception e) {
                Log.w(TAG, "update: install failed", e);
                ready = null;
                apk.delete();
                state = "failed " + e.getMessage();
            } finally {
                busy.set(false);
            }
        }, "updater").start();
    }

    /** From UpdateReceiver: the installer said no, or the person cancelled. The
     *  download is kept; the next tap or leave tries again. */
    static void notInstalled() {
        wanted = false;
        state = null;
    }

    /** The newer build, downloaded and verified, or null when there is none. */
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
            state = "fetching";
            download(BASE + "cartopolis.apk", apk);
            if (!want.equals(sha256(apk))) {
                apk.delete();
                throw new IOException("the download does not match its published sha256");
            }
        }
        PackageManager pm = app.getPackageManager();
        PackageInfo info = pm.getPackageArchiveInfo(apk.getPath(), PackageManager.GET_SIGNING_CERTIFICATES);
        if (info == null || !app.getPackageName().equals(info.packageName)
                || info.getLongVersionCode() <= own) {
            apk.delete();
            throw new IOException("the download is not a newer Cartopolis");
        }
        Signature[] theirs = signers(info.signingInfo);
        if (theirs == null) {
            Log.w(TAG, "update: this Android cannot read the download's signer; its own check decides");
        } else if (!Arrays.equals(theirs, ownSigners(app))) {
            apk.delete();
            throw new IOException("the download is not signed with this app's key");
        }
        readyName = version.optString("name", "build " + code);
        return apk;
    }

    private static Signature[] signers(SigningInfo signing) {
        if (signing == null) {
            return null;
        }
        Signature[] s = signing.getApkContentsSigners();
        return s == null || s.length == 0 ? null : s;
    }

    private static Signature[] ownSigners(Context app) throws IOException {
        try {
            PackageInfo own = app.getPackageManager().getPackageInfo(
                    app.getPackageName(), PackageManager.GET_SIGNING_CERTIFICATES);
            Signature[] s = signers(own.signingInfo);
            if (s == null) {
                throw new IOException("cannot read this app's own signer");
            }
            return s;
        } catch (PackageManager.NameNotFoundException e) {
            throw new IOException(e);
        }
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
                copy(in, out, Long.MAX_VALUE);
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

    static SharedPreferences prefs(Context app) {
        return app.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
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
        // Only the answer from cartopolis.org itself, never one it points at.
        http.setInstanceFollowRedirects(false);
        http.setConnectTimeout(15_000);
        http.setReadTimeout(30_000);
        http.setUseCaches(false);
        if (http.getResponseCode() != 200) {
            int code = http.getResponseCode();
            http.disconnect();
            throw new IOException(url + ": HTTP " + code);
        }
        return http;
    }

    private static String text(String url) throws IOException {
        HttpURLConnection http = open(url);
        try (InputStream in = http.getInputStream()) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            copy(in, out, 64 * 1024);
            return out.toString(StandardCharsets.UTF_8.name());
        } finally {
            http.disconnect();
        }
    }

    private static void download(String url, File to) throws IOException {
        File part = new File(to.getPath() + ".part");
        HttpURLConnection http = open(url);
        long length = http.getContentLengthLong();
        if (length > MAX_BYTES) {
            http.disconnect();
            throw new IOException("the APK is " + length + " bytes, over the cap");
        }
        total = Math.max(length, 0);
        done = 0;
        try (InputStream in = http.getInputStream(); OutputStream out = new FileOutputStream(part)) {
            copy(in, out, MAX_BYTES);
        } catch (IOException e) {
            part.delete();
            throw e;
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

    /** Copies at most `cap` bytes, counting them into `done` for the progress bar. */
    private static void copy(InputStream in, OutputStream out, long cap) throws IOException {
        byte[] buf = new byte[1 << 16];
        long n = 0;
        for (int r; (r = in.read(buf)) > 0; ) {
            n += r;
            if (n > cap) {
                throw new IOException("more than " + cap + " bytes");
            }
            out.write(buf, 0, r);
            done = n;
        }
    }
}

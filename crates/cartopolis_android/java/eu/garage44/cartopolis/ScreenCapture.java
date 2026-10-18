// Sharing the phone's screen in a call (`platform::screen` on the Rust side,
// `docs/notes/calls.md`).
//
// Android hands the screen only to an app the person said yes to, through
// MediaProjection, and only in a particular order since Android 14 (API 34):
//
//   1. `start` shows the system's "Start recording or casting?" dialog
//      (`createScreenCaptureIntent`), from the UI thread;
//   2. its answer reaches `MainActivity.onActivityResult`, which hands it to
//      `onResult`;
//   3. that starts `ScreenService`, a foreground service of type
//      `mediaProjection` — the projection may only be created while one runs;
//   4. the service, once in the foreground, calls `begin`, which makes the
//      projection, a virtual display mirroring the screen, and an ImageReader
//      the display draws into.
//
// The token in the dialog's answer is good for one projection: sharing again
// asks again. Pictures are kept here, newest only and a few a second, and Rust
// polls for them (`latest`): the display draws at the screen's refresh rate,
// and a call needs two or three pictures a second, not sixty copies.

package eu.garage44.cartopolis;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.graphics.PixelFormat;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionManager;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.util.DisplayMetrics;

import java.nio.ByteBuffer;

public final class ScreenCapture {
    /** The request code the dialog's answer comes back under. */
    static final int REQUEST = 0x5C4E;

    /** The longest side of what is captured: what a receiver decodes. */
    private static final int MAX_SIDE = 1280;
    /** The least time between two pictures kept, ms. */
    private static final long EVERY_MS = 350;

    private static final Object LOCK = new Object();
    private static MediaProjection projection;
    private static VirtualDisplay display;
    private static ImageReader reader;
    private static HandlerThread thread;
    private static byte[] frame;
    private static int width;
    private static int height;
    private static int stride;
    private static long frames;
    private static long keptAt;
    /** null before anything happened; "asking", "running", "stopped", or why not. */
    private static volatile String status;

    private ScreenCapture() {}

    /** Ask the person for the screen. Any thread. */
    static void start(Activity activity) {
        status = "asking";
        activity.runOnUiThread(() -> {
            try {
                MediaProjectionManager manager =
                        activity.getSystemService(MediaProjectionManager.class);
                activity.startActivityForResult(manager.createScreenCaptureIntent(), REQUEST);
            } catch (RuntimeException e) {
                status = "This phone would not share its screen: " + e.getMessage();
            }
        });
    }

    /** The dialog's answer, from `MainActivity.onActivityResult`. */
    static void onResult(Activity activity, int resultCode, Intent data) {
        if (resultCode != Activity.RESULT_OK || data == null) {
            status = "You did not share the screen";
            return;
        }
        Intent service = new Intent(activity, ScreenService.class);
        service.putExtra("code", resultCode);
        service.putExtra("data", data);
        try {
            activity.startForegroundService(service);
        } catch (RuntimeException e) {
            status = "Could not start sharing: " + e.getMessage();
        }
    }

    /** From `ScreenService`, once it is in the foreground. */
    static void begin(Context context, int code, Intent data) {
        try {
            MediaProjectionManager manager =
                    context.getSystemService(MediaProjectionManager.class);
            MediaProjection p = manager.getMediaProjection(code, data);
            if (p == null) {
                status = "Android refused the screen";
                ScreenService.stop(context);
                return;
            }
            DisplayMetrics metrics = context.getResources().getDisplayMetrics();
            float scale = Math.min(1f,
                    (float) MAX_SIDE / Math.max(metrics.widthPixels, metrics.heightPixels));
            int w = Math.max(2, Math.round(metrics.widthPixels * scale) & ~1);
            int h = Math.max(2, Math.round(metrics.heightPixels * scale) & ~1);

            HandlerThread t = new HandlerThread("screen-capture");
            t.start();
            Handler handler = new Handler(t.getLooper());
            ImageReader r = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 2);
            r.setOnImageAvailableListener(ScreenCapture::keep, handler);
            // Required before the display since API 34: the projection says
            // when the person stopped it from the system's own controls.
            p.registerCallback(new MediaProjection.Callback() {
                @Override
                public void onStop() {
                    stop(context);
                }
            }, handler);
            VirtualDisplay d = p.createVirtualDisplay("cartopolis-screen", w, h,
                    metrics.densityDpi, DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    r.getSurface(), null, handler);
            synchronized (LOCK) {
                projection = p;
                reader = r;
                display = d;
                thread = t;
                frames = 0;
                frame = null;
            }
            status = "running";
        } catch (RuntimeException e) {
            status = "Could not share the screen: " + e.getMessage();
            ScreenService.stop(context);
        }
    }

    /** The newest picture, kept if the last one was long enough ago. */
    private static void keep(ImageReader r) {
        Image image = r.acquireLatestImage();
        if (image == null) {
            return;
        }
        try {
            long now = SystemClock.uptimeMillis();
            synchronized (LOCK) {
                if (now - keptAt < EVERY_MS) {
                    return;
                }
                keptAt = now;
                Image.Plane plane = image.getPlanes()[0];
                ByteBuffer pixels = plane.getBuffer();
                int need = pixels.remaining();
                if (frame == null || frame.length != need) {
                    frame = new byte[need];
                }
                pixels.get(frame, 0, need);
                width = image.getWidth();
                height = image.getHeight();
                stride = plane.getRowStride();
                frames++;
            }
        } finally {
            image.close();
        }
    }

    /** Stop sharing. Any thread; also called when the person stops it. */
    static void stop(Context context) {
        synchronized (LOCK) {
            if (display != null) {
                display.release();
                display = null;
            }
            if (projection != null) {
                projection.stop();
                projection = null;
            }
            if (reader != null) {
                reader.close();
                reader = null;
            }
            if (thread != null) {
                thread.quitSafely();
                thread = null;
            }
            frame = null;
        }
        if (!"asking".equals(status)) {
            status = "stopped";
        }
        ScreenService.stop(context);
    }

    static String status() {
        return status;
    }

    /** `{width, height, stride, pictures so far}`. */
    static long[] info() {
        synchronized (LOCK) {
            return new long[] {width, height, stride, frames};
        }
    }

    /** A copy of the newest picture, RGBA rows `stride` bytes apart, or null. */
    static byte[] latest() {
        synchronized (LOCK) {
            return frame == null ? null : frame.clone();
        }
    }
}

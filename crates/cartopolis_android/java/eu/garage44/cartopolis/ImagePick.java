// Picking a picture that is already on the phone (`platform::photo`'s
// `Source::File` on Android).
//
// The system's own picker is an activity, and its answer comes back only
// through `onActivityResult`, which `android-activity` never surfaces to Rust:
// that is why the Android arm of `platform::photo` could take a photo with the
// camera app (watched through the MediaStore row it writes into) but never pick
// one. `MainActivity.onActivityResult` hands the answer here, the bytes are read
// on a thread of their own, and Rust polls `status` and `take`.
//
// The bytes are the file as stored, EXIF included as far as Android lets an app
// see it: the documents UI may leave out the GPS position of a photo it serves.

package eu.garage44.cartopolis;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;

public final class ImagePick {
    /** The request code the picker's answer comes back under. */
    static final int REQUEST = 0x1A6E;
    /** The largest file read, bytes: a phone photo is a few MB. */
    private static final int MAX_BYTES = 40 * 1024 * 1024;

    private static final Object LOCK = new Object();
    private static byte[] picked;
    /** null before anything happened; "picking", "ready", "cancelled", or why not. */
    private static volatile String status;

    private ImagePick() {}

    /** Open the system's picker for one image. Any thread. */
    static void start(Activity activity) {
        synchronized (LOCK) {
            picked = null;
        }
        status = "picking";
        activity.runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_GET_CONTENT);
                intent.setType("image/*");
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                activity.startActivityForResult(intent, REQUEST);
            } catch (RuntimeException e) {
                status = "This phone has no picture picker: " + e.getMessage();
            }
        });
    }

    /** The picker's answer, from `MainActivity.onActivityResult`. */
    static void onResult(Activity activity, int resultCode, Intent data) {
        Uri uri = data == null ? null : data.getData();
        if (resultCode != Activity.RESULT_OK || uri == null) {
            status = "cancelled";
            return;
        }
        Thread reader = new Thread(() -> {
            try (InputStream in = activity.getContentResolver().openInputStream(uri)) {
                if (in == null) {
                    status = "The picture could not be opened";
                    return;
                }
                ByteArrayOutputStream out = new ByteArrayOutputStream();
                byte[] buf = new byte[64 * 1024];
                int n;
                while ((n = in.read(buf)) > 0) {
                    out.write(buf, 0, n);
                    if (out.size() > MAX_BYTES) {
                        status = "That picture is too large";
                        return;
                    }
                }
                synchronized (LOCK) {
                    picked = out.toByteArray();
                }
                status = "ready";
            } catch (Exception e) {
                status = "The picture could not be read: " + e.getMessage();
            }
        }, "cartopolis-image-pick");
        reader.start();
    }

    static String status() {
        return status;
    }

    /** The picked file, once; clears the status with it. */
    static byte[] take() {
        synchronized (LOCK) {
            byte[] out = picked;
            picked = null;
            if (out != null) {
                status = null;
            }
            return out;
        }
    }

    /** Forget an answer nobody wants any more. */
    static void reset() {
        synchronized (LOCK) {
            picked = null;
        }
        status = null;
    }
}

// The first Java in this APK, and the reason `classes.dex` exists at all.
//
// Until now the package was deliberately dex-free: `android.app.NativeActivity`
// is a framework class, so a manifest plus a `.so` was a complete app and no
// Java ever had to be compiled (`docs/notes/android-build.md`). That ruled out
// everything the platform only offers to a *class* — a foreground service, an
// intent trampoline, the soft keyboard's InputConnection — and each of those was
// written up as a dead end rather than as work.
//
// It is not a dead end: `d8` ships in the same build-tools directory the build
// already takes `aapt2`, `zipalign` and `apksigner` from, and `javac` comes with
// the JDK that `apksigner` and `keytool` already require. So the lane costs two
// commands in `build-apk.sh` and no new tooling — no Gradle, no Android Gradle
// Plugin, no AAR resolution, and (the point of doing it this way) no Google Play
// services anywhere in the dependency graph.
//
// This class exists to prove that lane end to end, and to keep proving it. It is
// a diagnostic, not a placeholder: what it establishes is not "a dex is in the
// APK" — `unzip -l` answers that — but that the class is *reachable from the
// thread Rust actually runs on*, which is a different and much sharper question.
// See `platform::android::dex_probe` for why, and for the classloader dance that
// turns out to be required.

package eu.garage44.cartopolis;

/** A single static method, called once at startup from `platform::android`. */
public final class DexProbe {
    /** Not instantiable: every member is static and the class is a probe. */
    private DexProbe() {}

    /**
     * Return something only a loaded, linked, executing class could produce.
     *
     * `Build.VERSION.SDK_INT` is read on purpose rather than returning a
     * constant string: a constant would be folded into the dex as a literal and
     * a successful call would prove only that the *string pool* was reachable.
     * Reading a framework field means the class was resolved against the real
     * `android.jar` at build time and linked against the real framework at run
     * time — which is exactly the chain a `Service` subclass will depend on.
     */
    public static String probe() {
        return "DexProbe on API " + android.os.Build.VERSION.SDK_INT;
    }
}

// The owner's release key: what approves a release of the app and of the
// self-hosted server (docs/mockups/release-approval).
//
// ## What it is
//
// An ECDSA P-256 key in the Android Keystore, made on this phone and never able
// to leave it: in StrongBox (the separate security chip) where the phone has
// one, in the TEE otherwise. Every signature needs a *strong* biometric — a
// fingerprint, not a PIN or pattern — through Android's own BiometricPrompt
// sheet, and the key is invalidated when a new fingerprint is enrolled, so
// adding a finger to a stolen, unlocked phone does not make it sign.
//
// A session driving the app through the Midas link can ask for a signature and
// open the sheet; it cannot answer it.
//
// ## How Rust uses it
//
// `create` makes the key if there is none and gives the public half; `sign`
// opens the sheet for some bytes and returns at once; `status` is polled for
// the outcome, as `Updater.status` is:
//
//   null              nothing asked
//   "signing"         the sheet is up
//   "signed <base64>" the ASN.1 signature (SHA256withECDSA), what
//                     crates/server/src/update.rs and the APK updater check
//   "cancelled"       the person said no
//   "failed <why>"
//
// The public key is given as the 65-byte uncompressed point in hex: the form
// `RELEASE_KEYS` holds.

package eu.garage44.cartopolis;

import android.app.Activity;
import android.content.pm.PackageManager;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Build;
import android.os.CancellationSignal;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyInfo;
import android.security.keystore.KeyProperties;
import android.security.keystore.StrongBoxUnavailableException;
import android.util.Base64;
import android.util.Log;

import java.math.BigInteger;
import java.security.KeyFactory;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.PrivateKey;
import java.security.Signature;
import java.security.cert.Certificate;
import java.security.interfaces.ECPublicKey;
import java.security.spec.ECGenParameterSpec;

public final class ReleaseKey {
    private static final String TAG = "cartopolis";
    private static final String STORE = "AndroidKeyStore";
    static final String ALIAS = "cartopolis-release";

    private static String status = null;

    private ReleaseKey() {}

    public static synchronized String status() {
        return status;
    }

    private static synchronized void set(String s) {
        status = s;
    }

    /** The public key in hex, or null when this phone has no release key. */
    public static String publicKey() {
        try {
            KeyStore ks = KeyStore.getInstance(STORE);
            ks.load(null);
            Certificate cert = ks.getCertificate(ALIAS);
            return cert == null ? null : hexPoint((ECPublicKey) cert.getPublicKey());
        } catch (Exception e) {
            Log.w(TAG, "release key: " + e);
            return null;
        }
    }

    /**
     * Where the key lives: "strongbox", "tee", or "software" (which a phone
     * should never answer; the key is then not worth signing with).
     */
    public static String where() {
        try {
            KeyStore ks = KeyStore.getInstance(STORE);
            ks.load(null);
            PrivateKey key = (PrivateKey) ks.getKey(ALIAS, null);
            if (key == null) return null;
            KeyInfo info = KeyFactory.getInstance(key.getAlgorithm(), STORE).getKeySpec(key, KeyInfo.class);
            if (Build.VERSION.SDK_INT >= 31) {
                int level = info.getSecurityLevel();
                if (level == KeyProperties.SECURITY_LEVEL_STRONGBOX) return "strongbox";
                if (level == KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT) return "tee";
                return "software";
            }
            return info.isInsideSecureHardware() ? "tee" : "software";
        } catch (Exception e) {
            return null;
        }
    }

    /** Make the key if this phone has none; its public key in hex, or "error <why>". */
    public static String create(Activity activity) {
        String existing = publicKey();
        if (existing != null) return existing;
        boolean strongBox = Build.VERSION.SDK_INT >= 28
                && activity.getPackageManager().hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE);
        try {
            generate(strongBox);
        } catch (StrongBoxUnavailableException e) {
            try {
                generate(false);
            } catch (Exception e2) {
                return "error " + e2;
            }
        } catch (Exception e) {
            return "error " + e;
        }
        String made = publicKey();
        Log.i(TAG, "release key made, in " + where());
        return made == null ? "error the key was made but cannot be read back" : made;
    }

    private static void generate(boolean strongBox) throws Exception {
        KeyGenParameterSpec.Builder b = new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_SIGN)
                .setAlgorithmParameterSpec(new ECGenParameterSpec("secp256r1"))
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setUserAuthenticationRequired(true)
                .setInvalidatedByBiometricEnrollment(true);
        if (Build.VERSION.SDK_INT >= 30) {
            // Every use, and only a strong biometric: no PIN, no pattern.
            b.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG);
        } else {
            b.setUserAuthenticationValidityDurationSeconds(-1);
        }
        if (strongBox) b.setIsStrongBoxBacked(true);
        KeyPairGenerator g = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, STORE);
        g.initialize(b.build());
        g.generateKeyPair();
    }

    /**
     * Ask for a fingerprint, then sign `data`. Returns at once; `status` says
     * how it went. `title` and `what` are what the sheet says it is for.
     */
    public static void sign(final Activity activity, final byte[] data, final String title, final String what) {
        set("signing");
        activity.runOnUiThread(() -> {
            try {
                KeyStore ks = KeyStore.getInstance(STORE);
                ks.load(null);
                PrivateKey key = (PrivateKey) ks.getKey(ALIAS, null);
                if (key == null) {
                    set("failed this phone has no release key");
                    return;
                }
                Signature signature = Signature.getInstance("SHA256withECDSA");
                signature.initSign(key);
                BiometricPrompt.Builder builder = new BiometricPrompt.Builder(activity)
                        .setTitle(title)
                        .setDescription(what)
                        .setNegativeButton("Cancel", activity.getMainExecutor(),
                                (dialog, which) -> set("cancelled"));
                if (Build.VERSION.SDK_INT >= 30) {
                    builder.setAllowedAuthenticators(android.hardware.biometrics.BiometricManager.Authenticators.BIOMETRIC_STRONG);
                }
                builder.build().authenticate(
                        new BiometricPrompt.CryptoObject(signature),
                        new CancellationSignal(),
                        activity.getMainExecutor(),
                        new BiometricPrompt.AuthenticationCallback() {
                            @Override
                            public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                                try {
                                    Signature s = result.getCryptoObject().getSignature();
                                    s.update(data);
                                    set("signed " + Base64.encodeToString(s.sign(), Base64.NO_WRAP));
                                } catch (Exception e) {
                                    set("failed " + e);
                                }
                            }

                            @Override
                            public void onAuthenticationError(int code, CharSequence message) {
                                if (code == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED
                                        || code == BiometricPrompt.BIOMETRIC_ERROR_CANCELED) {
                                    set("cancelled");
                                } else {
                                    set("failed " + message);
                                }
                            }
                        });
            } catch (android.security.keystore.KeyPermanentlyInvalidatedException e) {
                set("failed the key was invalidated by a new fingerprint; make a new one");
            } catch (Exception e) {
                set("failed " + e);
            }
        });
    }

    /** Forget the outcome once Rust has read it. */
    public static synchronized void clear() {
        status = null;
    }

    private static String hexPoint(ECPublicKey key) {
        byte[] out = new byte[65];
        out[0] = 4;
        copy32(key.getW().getAffineX(), out, 1);
        copy32(key.getW().getAffineY(), out, 33);
        StringBuilder sb = new StringBuilder(130);
        for (byte b : out) sb.append(String.format("%02x", b & 0xff));
        return sb.toString();
    }

    private static void copy32(BigInteger n, byte[] out, int at) {
        byte[] b = n.toByteArray();
        int len = Math.min(b.length, 32);
        System.arraycopy(b, b.length - len, out, at + 32 - len, len);
    }
}

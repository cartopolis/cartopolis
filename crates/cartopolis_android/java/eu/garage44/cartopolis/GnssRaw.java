// The phone's raw GNSS measurements, for the high-precision mode
// (`platform::gnss_raw`, docs/notes/high-precision-mode.md).
//
// Android hands these out only through a callback object — an abstract class,
// `GnssMeasurementsEvent.Callback` — which is why this is Java at all: a class
// extending a framework type is what JNI from Rust cannot make. Everything
// else stays in Rust. The callback packs each measurement into a row of longs
// and Rust drains the rows; nothing is interpreted here.
//
// Full tracking is asked for (API 31+, `GnssMeasurementRequest`): with duty
// cycling on, the chip's clock jumps between bursts and the carrier phase
// breaks at every one, so a test of whether this phone reports usable phase
// would be a test of the power saver instead.

package eu.garage44.cartopolis;

import android.content.Context;
import android.location.GnssClock;
import android.location.GnssMeasurement;
import android.location.GnssMeasurementRequest;
import android.location.GnssMeasurementsEvent;
import android.location.LocationManager;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;

import java.util.concurrent.Executor;
import java.util.concurrent.Executors;

public final class GnssRaw {
    /**
     * Longs per measurement row. The layout is read by
     * `platform::gnss_raw::RawMeasurement::from_row` and must change with it;
     * a double travels as its raw bits.
     */
    static final int STRIDE = 21;

    /** Rows kept for a reader that has not come back: about a minute. */
    private static final int MAX_ROWS = 4000;

    private static final Object LOCK = new Object();
    private static long[] rows = new long[STRIDE * 256];
    private static int count;
    private static int status = -1;
    private static Callback callback;
    private static Executor executor;

    private GnssRaw() {}

    private static final class Callback extends GnssMeasurementsEvent.Callback {
        @Override
        public void onGnssMeasurementsReceived(GnssMeasurementsEvent event) {
            GnssClock clock = event.getClock();
            long elapsed = Build.VERSION.SDK_INT >= 29 && clock.hasElapsedRealtimeNanos()
                    ? clock.getElapsedRealtimeNanos()
                    : 0L;
            long fullBias = clock.hasFullBiasNanos() ? clock.getFullBiasNanos() : 0L;
            double bias = clock.hasBiasNanos() ? clock.getBiasNanos() : 0.0;
            synchronized (LOCK) {
                for (GnssMeasurement m : event.getMeasurements()) {
                    if (count >= MAX_ROWS) {
                        System.arraycopy(rows, STRIDE, rows, 0, (count - 1) * STRIDE);
                        count--;
                    }
                    int need = (count + 1) * STRIDE;
                    if (need > rows.length) {
                        long[] grown = new long[Math.min(rows.length * 2, MAX_ROWS * STRIDE)];
                        System.arraycopy(rows, 0, grown, 0, count * STRIDE);
                        rows = grown;
                    }
                    int o = count * STRIDE;
                    rows[o] = elapsed;
                    rows[o + 1] = clock.getTimeNanos();
                    rows[o + 2] = fullBias;
                    rows[o + 3] = Double.doubleToRawLongBits(bias);
                    rows[o + 4] = clock.getHardwareClockDiscontinuityCount();
                    rows[o + 5] = m.getConstellationType();
                    rows[o + 6] = m.getSvid();
                    rows[o + 7] = Double.doubleToRawLongBits(
                            m.hasCarrierFrequencyHz() ? m.getCarrierFrequencyHz() : 0.0);
                    rows[o + 8] = Double.doubleToRawLongBits(m.getCn0DbHz());
                    rows[o + 9] = m.getState();
                    rows[o + 10] = m.getReceivedSvTimeNanos();
                    rows[o + 11] = m.getReceivedSvTimeUncertaintyNanos();
                    rows[o + 12] = Double.doubleToRawLongBits(m.getTimeOffsetNanos());
                    rows[o + 13] = Double.doubleToRawLongBits(m.getPseudorangeRateMetersPerSecond());
                    rows[o + 14] = Double.doubleToRawLongBits(
                            m.getPseudorangeRateUncertaintyMetersPerSecond());
                    rows[o + 15] = m.getAccumulatedDeltaRangeState();
                    rows[o + 16] = Double.doubleToRawLongBits(m.getAccumulatedDeltaRangeMeters());
                    rows[o + 17] = Double.doubleToRawLongBits(
                            m.getAccumulatedDeltaRangeUncertaintyMeters());
                    rows[o + 18] = m.getMultipathIndicator();
                    rows[o + 19] = Build.VERSION.SDK_INT >= 26 && m.hasAutomaticGainControlLevelDb()
                            ? Double.doubleToRawLongBits(m.getAutomaticGainControlLevelDb())
                            : Double.doubleToRawLongBits(Double.NaN);
                    rows[o + 20] = clock.hasDriftNanosPerSecond()
                            ? Double.doubleToRawLongBits(clock.getDriftNanosPerSecond())
                            : Double.doubleToRawLongBits(Double.NaN);
                    count++;
                }
            }
        }

        @Override
        public void onStatusChanged(int s) {
            synchronized (LOCK) {
                status = s;
            }
        }
    }

    /**
     * Start listening. Returns null when registered, or why not — most often
     * that the location permission has not been granted yet, which the caller
     * answers by trying again later.
     */
    public static String start(Context context) {
        synchronized (LOCK) {
            if (callback != null) {
                return null;
            }
        }
        LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
        if (lm == null) {
            return "no location service";
        }
        Callback cb = new Callback();
        boolean ok;
        try {
            if (Build.VERSION.SDK_INT >= 31) {
                if (executor == null) {
                    executor = Executors.newSingleThreadExecutor();
                }
                GnssMeasurementRequest request =
                        new GnssMeasurementRequest.Builder().setFullTracking(true).build();
                ok = lm.registerGnssMeasurementsCallback(request, executor, cb);
            } else {
                ok = lm.registerGnssMeasurementsCallback(cb, new Handler(Looper.getMainLooper()));
            }
        } catch (SecurityException e) {
            return "no location permission";
        } catch (RuntimeException e) {
            return e.toString();
        }
        if (!ok) {
            return "the location service refused the measurement listener";
        }
        synchronized (LOCK) {
            callback = cb;
        }
        return null;
    }

    public static void stop(Context context) {
        Callback cb;
        synchronized (LOCK) {
            cb = callback;
            callback = null;
            count = 0;
            status = -1;
        }
        if (cb == null) {
            return;
        }
        LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
        if (lm != null) {
            lm.unregisterGnssMeasurementsCallback(cb);
        }
    }

    /** Every row since the last call, oldest first, `STRIDE` longs each. */
    public static long[] drain() {
        synchronized (LOCK) {
            long[] out = new long[count * STRIDE];
            System.arraycopy(rows, 0, out, 0, out.length);
            count = 0;
            return out;
        }
    }

    /**
     * What the callback last said about the listener:
     * `GnssMeasurementsEvent.Callback.STATUS_*` (0 not supported, 1 ready,
     * 2 location off), or -1 before it said anything.
     */
    public static int status() {
        synchronized (LOCK) {
            return status;
        }
    }

    /** The GNSS chip, as the platform names it (API 28+). */
    public static String hardware(Context context) {
        LocationManager lm = (LocationManager) context.getSystemService(Context.LOCATION_SERVICE);
        if (lm == null) {
            return "";
        }
        String name = lm.getGnssHardwareModelName();
        StringBuilder s = new StringBuilder();
        s.append(name == null ? "unnamed" : name)
                .append(" (").append(lm.getGnssYearOfHardware()).append(")")
                .append(" · API ").append(Build.VERSION.SDK_INT);
        // What the platform says the chip can do. The phase capability is
        // only declared from API 34, and a driver may claim it and deliver
        // nothing, which is why the mode counts valid phase rather than
        // trusting this.
        if (Build.VERSION.SDK_INT >= 31) {
            android.location.GnssCapabilities caps = lm.getGnssCapabilities();
            s.append(" · measurements ").append(caps.hasMeasurements() ? "yes" : "no")
                    .append(" · navigation messages ")
                    .append(caps.hasNavigationMessages() ? "yes" : "no")
                    .append(" · antenna info ").append(caps.hasAntennaInfo() ? "yes" : "no");
            if (Build.VERSION.SDK_INT >= 34) {
                int adr = caps.hasAccumulatedDeltaRange();
                s.append(" · carrier phase ").append(
                        adr == 1 ? "yes" : adr == 2 ? "no" : "unknown");
            }
        }
        return s.toString();
    }
}

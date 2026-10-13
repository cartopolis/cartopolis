// A USB serial adapter through Android's USB host API — how an external GNSS
// receiver (an RTK rover on a USB-C cable) reaches the app on a phone
// (`platform::gnss_receiver`, docs/notes/rtk-receiver.md).
//
// ## Why this is Java, and why the driver is in here
//
// A phone gives an app no tty for a USB serial device: the kernel's FTDI and
// CDC-ACM drivers are not bound for apps, and the device is reached as raw USB
// through `UsbManager` — control transfers to set the line up, bulk transfers
// for the bytes. So the adapter's protocol has to be spoken here, and three
// cover the GNSS boards in circulation: FTDI (the FT230X on ArduSimple boards,
// among many), Silicon Labs CP210x, and plain CDC-ACM (receivers with a USB
// port of their own). WCH's CH34x is common on cheap boards and is *not* here;
// its line setup is a sequence of undocumented register writes, and it is said
// so by name rather than failing as "no adapter".
//
// The permission answer is a broadcast, which needs a `BroadcastReceiver`
// subclass — the other reason this cannot be JNI from Rust. Rust calls `open`
// about once a second until it returns null; between the request and the
// answer it returns a sentence saying what it is waiting for.
//
// Everything is static and locked: there is one receiver, and one Rust thread
// reading it.
//
// ## Nothing here can change the adapter for good
//
// Every request below sets volatile line state — baud, framing, the modem
// lines — which the chip forgets when unplugged. The requests that would not
// be (FTDI's EEPROM write and erase, 0x91/0x92; Silicon Labs' vendor
// configuration writes behind 0xFF) are never built, and `control` refuses
// anything outside the short list per adapter in `allowed`, so a later edit
// cannot add one by accident. What reaches the *receiver* is guarded on the
// Rust side (`cartopolis_gnss::receiver`).

package eu.garage44.cartopolis;

import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.hardware.usb.UsbConstants;
import android.hardware.usb.UsbDevice;
import android.hardware.usb.UsbDeviceConnection;
import android.hardware.usb.UsbEndpoint;
import android.hardware.usb.UsbInterface;
import android.hardware.usb.UsbManager;
import android.os.Build;

public final class UsbSerial {
    private static final String ACTION_PERMISSION = "eu.garage44.cartopolis.USB_PERMISSION";
    private static final int TIMEOUT_MS = 500;

    private static final int FTDI = 1;
    private static final int CP210X = 2;
    private static final int CDC = 3;

    private static final Object LOCK = new Object();
    private static UsbDeviceConnection connection;
    private static UsbInterface dataInterface;
    private static UsbInterface controlInterface;
    private static UsbEndpoint in;
    private static UsbEndpoint out;
    private static int kind;
    private static String description = "";
    /** For telling a timeout from an unplug: the open device, and where to look. */
    private static String deviceName;
    private static UsbManager usb;
    private static byte[] readBuffer = new byte[4096];

    /** The device the permission was last asked for, and whether it was refused. */
    private static String asked;
    private static volatile boolean refused;
    private static BroadcastReceiver permissionReceiver;

    private UsbSerial() {}

    /**
     * Open the first supported adapter at `baud`, 8N1. Null when open; else a
     * sentence for the user (no adapter, waiting for permission, refused).
     */
    public static String open(Context context, int baud) {
        synchronized (LOCK) {
            closeLocked();
            Context app = context.getApplicationContext();
            UsbManager manager = (UsbManager) app.getSystemService(Context.USB_SERVICE);
            if (manager == null) {
                return "this device has no USB host support";
            }
            UsbDevice device = null;
            int found = 0;
            String unsupported = null;
            for (UsbDevice d : manager.getDeviceList().values()) {
                int k = classify(d);
                if (k != 0) {
                    device = d;
                    found = k;
                    break;
                }
                if (d.getVendorId() == 0x1A86) {
                    unsupported = "the receiver's USB adapter (WCH CH34x) is not supported yet";
                }
            }
            if (device == null) {
                asked = null;
                refused = false;
                return unsupported != null ? unsupported : "no USB receiver connected";
            }
            if (!manager.hasPermission(device)) {
                if (device.getDeviceName().equals(asked)) {
                    return refused
                            ? "USB access was refused — unplug the receiver and plug it in again to be asked again"
                            : "waiting for permission to use the USB receiver";
                }
                asked = device.getDeviceName();
                refused = false;
                requestPermission(app, manager, device);
                return "waiting for permission to use the USB receiver";
            }
            UsbDeviceConnection c = manager.openDevice(device);
            if (c == null) {
                return "could not open the USB receiver";
            }
            connection = c;
            kind = found;
            deviceName = device.getDeviceName();
            usb = manager;
            String why;
            try {
                why = setUp(device, baud);
            } catch (RuntimeException e) {
                why = e.toString();
            }
            if (why != null) {
                closeLocked();
                return why;
            }
            String name = device.getProductName();
            description = (name != null ? name : String.format("USB %04x:%04x",
                    device.getVendorId(), device.getProductId()))
                    + " (USB), " + baud + " baud";
            return null;
        }
    }

    /** What is open, for the settings readout. */
    public static String describe() {
        synchronized (LOCK) {
            return description;
        }
    }

    /**
     * Up to `max` bytes, waiting at most `timeoutMs`: an empty array when
     * nothing arrived, null when the device is gone.
     */
    public static byte[] read(int max, int timeoutMs) {
        synchronized (LOCK) {
            if (connection == null) {
                return null;
            }
            int packet = in.getMaxPacketSize();
            int want = Math.min(Math.min(max, readBuffer.length), packet * 8);
            if (kind == FTDI) {
                // Whole packets only: each one carries a two-byte modem status
                // header, which the split below depends on finding at a packet
                // boundary.
                want = Math.max(packet, want - want % packet);
            }
            long deadline = System.currentTimeMillis() + timeoutMs;
            while (true) {
                long left = Math.max(1, deadline - System.currentTimeMillis());
                int n = connection.bulkTransfer(in, readBuffer, want, (int) left);
                if (n < 0) {
                    // A timeout and an unplugged device both come back as -1;
                    // only the device list tells them apart.
                    return stillAttached() ? new byte[0] : null;
                }
                byte[] data = kind == FTDI ? stripFtdiStatus(n, packet) : copy(readBuffer, n);
                // An idle FTDI chip still answers every latency period, with a
                // packet that is all status and no data; wait those out here
                // rather than handing Rust a stream of empty reads.
                if (data.length > 0 || System.currentTimeMillis() >= deadline) {
                    return data;
                }
            }
        }
    }

    private static byte[] copy(byte[] from, int n) {
        byte[] data = new byte[n];
        System.arraycopy(from, 0, data, 0, n);
        return data;
    }

    /** Drop the two status bytes at the head of every packet in `readBuffer`. */
    private static byte[] stripFtdiStatus(int n, int packet) {
        byte[] data = new byte[n];
        int length = 0;
        for (int at = 0; at < n; at += packet) {
            int end = Math.min(at + packet, n);
            if (end - at > 2) {
                System.arraycopy(readBuffer, at + 2, data, length, end - at - 2);
                length += end - at - 2;
            }
        }
        return copy(data, length);
    }

    /** Write all of `data`; false when the device is gone. */
    public static boolean write(byte[] data) {
        synchronized (LOCK) {
            if (connection == null) {
                return false;
            }
            int at = 0;
            while (at < data.length) {
                int chunk = Math.min(data.length - at, out.getMaxPacketSize() * 8);
                byte[] part = data;
                if (at != 0 || chunk != data.length) {
                    part = new byte[chunk];
                    System.arraycopy(data, at, part, 0, chunk);
                }
                int n = connection.bulkTransfer(out, part, chunk, TIMEOUT_MS);
                if (n <= 0) {
                    return false;
                }
                at += n;
            }
            return true;
        }
    }

    public static void close() {
        synchronized (LOCK) {
            closeLocked();
        }
    }

    private static void closeLocked() {
        if (connection == null) {
            return;
        }
        try {
            if (kind == CP210X) {
                control(0x41, 0x00, 0, dataInterface.getId(), null);
            }
        } catch (RuntimeException ignored) {
            // Going away regardless.
        }
        if (dataInterface != null) {
            connection.releaseInterface(dataInterface);
        }
        if (controlInterface != null) {
            connection.releaseInterface(controlInterface);
        }
        connection.close();
        connection = null;
        dataInterface = null;
        controlInterface = null;
        in = null;
        out = null;
        description = "";
        deviceName = null;
    }

    private static boolean stillAttached() {
        return usb != null && deviceName != null && usb.getDeviceList().containsKey(deviceName);
    }

    private static int classify(UsbDevice d) {
        if (d.getVendorId() == 0x0403) {
            return FTDI;
        }
        if (d.getVendorId() == 0x10C4) {
            return CP210X;
        }
        for (int i = 0; i < d.getInterfaceCount(); i++) {
            if (d.getInterface(i).getInterfaceClass() == UsbConstants.USB_CLASS_CDC_DATA) {
                return CDC;
            }
        }
        return 0;
    }

    private static void requestPermission(Context app, UsbManager manager, UsbDevice device) {
        if (permissionReceiver == null) {
            permissionReceiver = new BroadcastReceiver() {
                @Override
                public void onReceive(Context c, Intent intent) {
                    if (!intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)) {
                        refused = true;
                    }
                }
            };
            IntentFilter filter = new IntentFilter(ACTION_PERMISSION);
            if (Build.VERSION.SDK_INT >= 33) {
                app.registerReceiver(permissionReceiver, filter, Context.RECEIVER_NOT_EXPORTED);
            } else {
                app.registerReceiver(permissionReceiver, filter);
            }
        }
        // Explicit (package set) and mutable: the system writes the device and
        // the answer into it as extras, and from API 34 a mutable intent must
        // name its package.
        Intent intent = new Intent(ACTION_PERMISSION).setPackage(app.getPackageName());
        int flags = Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0;
        PendingIntent pending = PendingIntent.getBroadcast(app, 0, intent, flags);
        manager.requestPermission(device, pending);
    }

    /** Claim interfaces, find the endpoints and set the line up. Null on success. */
    private static String setUp(UsbDevice device, int baud) {
        if (kind == CDC) {
            for (int i = 0; i < device.getInterfaceCount(); i++) {
                UsbInterface f = device.getInterface(i);
                if (f.getInterfaceClass() == UsbConstants.USB_CLASS_COMM && controlInterface == null) {
                    controlInterface = f;
                } else if (f.getInterfaceClass() == UsbConstants.USB_CLASS_CDC_DATA && dataInterface == null) {
                    dataInterface = f;
                }
            }
        } else {
            dataInterface = device.getInterface(0);
        }
        if (dataInterface == null) {
            return "the USB receiver has no data interface";
        }
        if (controlInterface != null && !connection.claimInterface(controlInterface, true)) {
            controlInterface = null;
        }
        if (!connection.claimInterface(dataInterface, true)) {
            return "the USB receiver is in use by another app";
        }
        for (int i = 0; i < dataInterface.getEndpointCount(); i++) {
            UsbEndpoint e = dataInterface.getEndpoint(i);
            if (e.getType() != UsbConstants.USB_ENDPOINT_XFER_BULK) {
                continue;
            }
            if (e.getDirection() == UsbConstants.USB_DIR_IN && in == null) {
                in = e;
            } else if (e.getDirection() == UsbConstants.USB_DIR_OUT && out == null) {
                out = e;
            }
        }
        if (in == null || out == null) {
            return "the USB receiver has no bulk endpoints";
        }
        switch (kind) {
            case FTDI:
                return ftdi(baud);
            case CP210X:
                return cp210x(baud);
            default:
                return cdc(baud);
        }
    }

    private static int control(int type, int request, int value, int index, byte[] data) {
        if (!allowed(kind, type, request)) {
            throw new IllegalArgumentException(String.format(
                    "refused USB control request 0x%02x/0x%02x for adapter kind %d",
                    type, request, kind));
        }
        return connection.controlTransfer(type, request, value, index, data,
                data == null ? 0 : data.length, TIMEOUT_MS);
    }

    /**
     * The control requests each adapter may be sent: volatile line settings
     * only (see the note at the top).
     */
    static boolean allowed(int kind, int type, int request) {
        switch (kind) {
            case FTDI:
                // reset, modem lines, flow control, baud, data format, latency
                return type == 0x40 && (request == 0x00 || request == 0x01 || request == 0x02
                        || request == 0x03 || request == 0x04 || request == 0x09);
            case CP210X:
                // enable, baud, line control, modem lines
                return type == 0x41 && (request == 0x00 || request == 0x1E || request == 0x03
                        || request == 0x07);
            case CDC:
                // SET_LINE_CODING, SET_CONTROL_LINE_STATE
                return type == 0x21 && (request == 0x20 || request == 0x22);
            default:
                return false;
        }
    }

    /**
     * FTDI's vendor requests (the Linux `ftdi_sio` driver is the reference):
     * reset, DTR/RTS up, no flow control, the baud divisor, 8N1, and the
     * chip's default 16 ms latency timer said explicitly — a board configured
     * otherwise by another program would otherwise keep that.
     */
    private static String ftdi(int baud) {
        int port = 1;
        if (control(0x40, 0x00, 0, port, null) < 0) {
            return "the FTDI adapter did not accept a reset";
        }
        control(0x40, 0x01, 0x0303, port, null);
        control(0x40, 0x02, 0, port, null);
        int divisor = ftdiDivisor(baud);
        if (control(0x40, 0x03, divisor & 0xFFFF, (divisor >> 16) & 0xFFFF, null) < 0) {
            return "the FTDI adapter refused " + baud + " baud";
        }
        control(0x40, 0x04, 0x0008, port, null);
        control(0x40, 0x09, 16, port, null);
        return null;
    }

    /** The 232BM-and-later divisor: eighths of the 3 MHz base, fraction encoded. */
    static int ftdiDivisor(int baud) {
        final int[] frac = {0, 3, 2, 4, 1, 5, 6, 7};
        int eighths = (24000000 + baud / 2) / baud;
        int divisor = (eighths >> 3) | (frac[eighths & 7] << 14);
        if (divisor == 1) {
            divisor = 0;
        } else if (divisor == 0x4001) {
            divisor = 1;
        }
        return divisor;
    }

    /** Silicon Labs' requests: enable, baud as 32 bits, 8N1, DTR/RTS up. */
    private static String cp210x(int baud) {
        int iface = dataInterface.getId();
        if (control(0x41, 0x00, 1, iface, null) < 0) {
            return "the CP210x adapter could not be enabled";
        }
        byte[] rate = {(byte) baud, (byte) (baud >> 8), (byte) (baud >> 16), (byte) (baud >> 24)};
        if (control(0x41, 0x1E, 0, iface, rate) < 0) {
            return "the CP210x adapter refused " + baud + " baud";
        }
        control(0x41, 0x03, 0x0800, iface, null);
        control(0x41, 0x07, 0x0303, iface, null);
        return null;
    }

    /** CDC-ACM: SET_LINE_CODING and SET_CONTROL_LINE_STATE, when there is a
     *  control interface to send them to (a receiver's own port often ignores
     *  both, so a refusal is not an error). */
    private static String cdc(int baud) {
        if (controlInterface == null) {
            return null;
        }
        int iface = controlInterface.getId();
        byte[] coding = {(byte) baud, (byte) (baud >> 8), (byte) (baud >> 16), (byte) (baud >> 24),
                0, 0, 8};
        control(0x21, 0x20, 0, iface, coding);
        control(0x21, 0x22, 0x0003, iface, null);
        return null;
    }
}

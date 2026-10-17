---
topic: rtk-receiver
triggers: [gnss_receiver, cartopolis_gnss, UsbSerial.java, RTK, NTRIP, UM980, RTCM, NMEA, ttyUSB, "external receiver", "serial port", FT230X, crates/gnss]
updated: 2026-10-17
---

# An external RTK receiver as the position

A GNSS receiver on a cable — first the UM980 RTK Handheld Surveyor Kit — is the
client's first location provider when switched on in Settings ▸ General ▸ GNSS
receiver. `platform::gnss_receiver` runs it; `LocationWatch` asks it before the
platform's own provider, so the locate button, driver mode, location sharing and
the high-precision anchor take its fix without knowing it exists. The parsing,
the caster client and the receiver loop are the Bevy-free `cartopolis_gnss`.
The crate reads any byte port; the client supplies one per platform — a tty
through the crate's `serial` feature on the desktop (off by default, and never
turned on for Android, which gets no tty for a USB adapter) and `UsbSerial.java`
on Android.

## The receiver does the RTK

The client never solves anything. It asks the receiver for GGA/RMC/GST
(`receiver::configure_commands`), writes the caster's RTCM into the same port,
and reads back a position whose GGA quality says single, DGPS, RTK float or RTK
fixed. The accuracy is the receiver's own GST (√(σlat² + σlng²), ≈ the 68 %
radius Android reports) and only estimated from quality and HDOP when no GST
arrives — so a fixed solution reads ±1–2 cm and the phone's ±4 m beside it
compare like for like.

## The kit as found (2026-10-13)

- Enumerates as **FTDI FT230X** (`0403:6015`) → `/dev/ttyUSB0`, owned
  `root:uucp 0660` on the Arch workstation. The account was **not in `uucp`**,
  so nothing could read it; the receiver's error line names the group
  (`sudo usermod -aG uucp $USER`, then log in again). Debian/Ubuntu call it
  `dialout`.
- **UM980, firmware R4.10 Build 20739** (`VERSIONA`). **115200 baud**, and out
  of the box it already writes GGA, RMC, GST, VTG, GSA and GSV at 1 Hz on the
  USB port (~35 sentences/s with GSV). A wrong baud shows as "Data is arriving,
  but none of it is NMEA".
- The configure commands omit the port name (`GPGGA 0.2`), which Unicore applies
  to the port the command arrived on; confirmed on this kit — all three answer
  `response: OK` and GGA comes at 5 Hz after. Nothing is `SAVECONFIG`ed.
- **Unicore's `$command` replies checksum the `$` too**, unlike every NMEA
  sentence beside them. The framer accepts that form for `command,` lines only;
  before it did, every reply counted as a bad checksum and a refused command
  could never have been shown.
- Indoors (2026-10-13): 11–13 satellites in the solution across GPS,
  GLONASS, Galileo and BeiDou on several bands each, C/N0 15–35 dB-Hz, single
  fix, GST accuracy ±18–30 m. Open sky is needed before RTK means anything.

## Nothing may change the receiver for good

A Unicore receiver takes text commands on the port it reports on, including
`FRESET` (factory reset), `SAVECONFIG`, baud and mode changes, and it is the
same port the corrections go into. So every write passes one of two gates in
`receiver.rs`, which check each byte they are handed:

- **Commands:** `allowed_command`, which admits exactly `GPGGA`/`GPRMC`/`GPGST`
  at 1, 0.5, 0.2, 0.1 or 0.05 s, one line, and nothing else. Never
  `SAVECONFIG`, so unplugging restores the receiver's own setup. The rate is
  clamped to 20 Hz (the UM980's position rate) and then to what the baud
  carries (`rate_for_baud`: 115200 → 20 Hz, 57600 → 10, 38400 → 5, 9600 → 1),
  because a line that cannot carry the rate makes the receiver drop sentences.
- **The baud is never sent to the receiver.** It sets this side of the link
  only; changing the receiver's own port speed is a configuration change this
  client does not make, since a wrong one cuts the link until fixed by hand.
  That is also why 50 Hz is not offered: it needs more than 115200.
- **Corrections:** `rtcm::checked_frame`, which admits one whole CRC-valid
  frame with a standard message number (1001–1299). The caster's raw bytes are
  **never** passed on: a wrong mountpoint answering with text, an HTML error
  page or a hostile caster would otherwise be typing at the receiver.
  Proprietary messages (4001–4095) are held back and counted.
- **Nothing is written until the receiver has said something checksummed at
  this baud.** A wrong baud setting therefore sends nothing at all, not even
  noise.
- **On Android**, `UsbSerial.java`'s `control` refuses any USB control request
  outside a per-adapter list of volatile line settings. The FTDI EEPROM writes
  (0x91/0x92) and Silicon Labs' configuration writes are never built and could
  not be sent.

`a_hostile_caster_cannot_type_at_the_receiver` and
`nothing_is_sent_to_a_receiver_that_has_not_been_heard` pin the first three.

## NTRIP

- Requests are revision-1 shaped (`HTTP/1.0`): every caster answers it, and a
  revision-2 request invites a chunked body that would have to be undone before
  the bytes reach the receiver.
- An empty mountpoint means **the nearest RTCM 3 station** from the
  sourcetable, measured from the receiver's first fix. Single-base RTK degrades
  with baseline length (tens of km), so this is the choice a person would make.
- The GGA uploaded is **rebuilt** (`gga_for_caster`): the UM980's own line is
  `GN`-talker with eight decimals of minutes, past NMEA's 82 bytes, which older
  network casters refuse.
- Sending the position is a setting (default on — VRS needs it, and it is what
  every NTRIP client does); it is also sent when an automatically chosen station
  says it wants it (`STR` field 11). The caster operator sees it.
- Passwords are stored in `user.json` in the clear, like the session tokens: the
  protocol sends them in the clear over plain TCP anyway.
- **Kadaster's caster is `ntrip-lab.kadaster.nl`, port 443, TLS only**
  (checked 2026-10-13: 80, 2101, 2102, 8080 and 49643 closed; 443 is an Azure
  gateway in front of a GNSMART caster, whose own `CAS` line says
  `127.0.0.1:49643`). That is why the client speaks TLS at all, and why port 443
  implies it. Settings has a one-tap preset for it.
- Its sourcetable (115 streams) has four networks. **AGRS.NL** (43 streams, the
  national reference stations) is `auth N`, `fee N`: open with no account.
  **NETPOS** (56) needs a login (`B`; network line `fee N`, stream lines
  `fee Y`). Each station is a pair: `…0` is RTCM 3.3, `…1` is RAW.
- Nearest open station to Groningen: **DELZ00NLD0** (Delfzijl tide gauge,
  ~26 km), then Westerbork (~35 km) and Schiermonnikoog (~40 km). Groningen's
  own NETPOS station GRZC00NLD9 (Zernike) has no position in the table, so it
  can only be chosen by name. Without a login the automatic choice skips
  login-only streams (`ntrip::nearest`).
- DELZ00NLD0 streamed ~2.4 kB/s: 1006, 1013, 1033, 1077/1087/1097/1127 (MSM7
  for GPS, GLONASS, Galileo, BeiDou) and 1230 — everything the UM980 needs.
- Revision-1 requests get `ICY 200 OK` and an unchunked body over that gateway;
  revision 2 gets `Transfer-Encoding: chunked`. The Host header is sent without
  `:443` (a gateway routes on it).
- **Kadaster is the default caster** (`NtripPrefs::default`), and `user.json`
  schema 2 moves a caster that was never filled in onto it. Outside the
  Netherlands this is harmless: every station is past `MAX_STATION_KM` (100 km),
  which is refused by name rather than connected to.
- **The station is chosen before the receiver has a fix**, from the map anchor
  (`Receiver::set_position_hint`). Waiting for the receiver's own first fix
  meant no corrections at all indoors, where a first fix can take the whole
  session. Once the receiver has a fix the choice is re-checked against it. The
  hint is never sent to the caster: GGA is only ever the receiver's.
- Community casters (RTK2go, Centipede) exist too.

## Getting a report off a phone

The receiver keeps its own event history (`Status::events`, the last 80:
opened, heard at this baud, configured, each reply, every change of fix kind,
the delivered rate, the station chosen and why, stream open, failures). A
phone's log is a ring this client rolls over in minutes, so the history is what
survives to be reported. Settings ▸ GNSS receiver ▸ *Copy receiver report* puts
settings (never the password), live state and the history on the clipboard;
*Share diagnostics…* carries the same section.

## Android: the USB host API, not a tty

A phone gives apps no tty for a USB serial adapter, so `UsbSerial.java` speaks
the adapter's protocol over `UsbDeviceConnection`: **FTDI**, **Silicon Labs
CP210x** and **CDC-ACM**. WCH **CH34x** is refused by name — its line setup is
undocumented register writes, and guessing them blind was not worth it. USB
access is asked per device at runtime (a broadcast; `Context.RECEIVER_NOT_EXPORTED`
works because the `PendingIntent` is ours). A refusal is remembered until the
device is unplugged, so the dialog is not re-raised every second.

## Rejected

- **gpsd / GeoClue2 on the desktop**: another daemon to install and configure,
  and neither carries corrections *into* the receiver; GeoClue also drops the
  fix quality, which is the thing an RTK user wants to see.
- **libusb (or `nusb`) over the Android `UsbDeviceConnection` fd**: works, but
  is a C library (or a new crate) plus usbdevfs ioctls from Rust, to replace
  ~300 lines of Java the framework already supports.
- **serialport's `libudev` feature**: a C `-dev` package on every Linux build
  host, for enumeration serialport also does from `/sys/class/tty`.

## Not verified

- **The Android path has never run on a device** — compiled only.
- **No RTK float or fixed yet** — only seen indoors. There (2026-10-13) the
  receiver went from no fix to **DGNSS with a 1.4 s correction age** 15 s after
  Kadaster's DELZ00NLD0 started streaming: the corrections are being used, but
  indoor signals (C/N0 15–35 dB-Hz, HDOP 4) allow nothing better.

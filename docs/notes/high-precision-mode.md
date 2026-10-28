---
topic: high-precision mode (camera localisation)
triggers: [high_precision, HighPrecision, HighPrecisionLog, precise_sensors, run_back_gray, "lens.rs", Pinhole, PreciseHand, "camera localiser", "visual localisation", VIO, "lens intrinsics", LENS_INTRINSIC_CALIBRATION, CLOCK_BOOTTIME, copy_text, "Developer tools ▸ Precision", gnss_raw, GnssRaw, RawGnss, RawStats, "raw GNSS", "carrier phase", accumulatedDeltaRange, RTK, NTRIP, NLGEO2018]
updated: 2026-10-07
---

# High-precision mode

A mode switched by the **hand button** above the face button in the touch
controls (`map_osd::hand_button`; until 2026-10-28 a checkbox in the locate
button's long-press menu, `docs/mockups/hand-tracking/`) that is
to give the phone's position to centimetres, so the view *is* the phone in your
hand. It is what makes a hand-sensing prim (`docs/notes/hands-and-tones.md`) — a
theremin on the Grote Markt — playable for real.

## How it is meant to work

GNSS alone is 2–5 m off in a city and the compass 5–15°; that is enough to say
"the theremin is 4 m ahead" and not enough to play it. So:

1. **Localise against the buildings.** The 3DBAG LoD2.2 models are already in
   the client. Gravity fixes pitch and roll to about a degree, and the terrain
   field fixes height (hand height above the ground), which leaves three
   unknowns — east, north, heading — inside the box GNSS and the compass give.
   Search them by projecting the model's outline (rooflines against the sky,
   building corners) at candidate poses and scoring against the edges in the
   camera frame. Rooflines are the robust cue: lighting, shop fronts and parked
   cars do not move them.
2. **Track between matches** with the gyroscope and accelerometer fused with
   corners followed frame to frame (visual-inertial odometry), so the pose is
   current at the motion sensors' 200 Hz and a match about once a second keeps
   it from drifting.
3. **Set `prim_hand::PreciseHand::active`** while a pose is held; that is the
   switch every hand-sensing script reads (`precise = true`).

Expected, revised after a literature pass (2026-10-07): **metres to decimetres
absolute, millimetres to centimetres relative over seconds.** Published
model-to-image localisation against LoD1/LoD2 city models reaches 1–3 m and
1–3° at street level, and nobody reports centimetres; the map itself bounds it
anyway — BAG footprints are ±30 cm, 3DBAG LoD2.2 roofs under 9 cm RMSE for 75 %
of buildings and under 31 cm for 95 %. What the outline match is strong at is
**heading** (a distant roofline fixes yaw to under a degree) and cross-street
position. The original estimate here was "10–30 cm absolute", which was a hope.
Netherlands only (3DBAG), daylight, buildings in frame.

**The theremin needs relative precision, not absolute.** Its pitch is 36
semitones over 55 cm (`theremin.lua`), about 1.5 cm a semitone, so playing in
tune wants millimetre-steady tracking and under ~1.5 cm of drift over a phrase.
An absolute error of 30 cm only shifts every note by the same amount, which a
player corrects by ear as on a real one; a correction arriving *mid-phrase* is
what ruins it. So the design to build towards is: visual-inertial tracking for
the relative pose, the slow sources (GNSS, compass, outline match) placing that
track on the map, and **no global correction applied to a hand while it is
playing** — re-anchor between phrases. Height is the terrain field plus a
per-session phone height (zero-velocity updates bound the drift), not GNSS,
whose height is 2–3× worse than its horizontal and on this phone was ~10 m off.

**ARCore was considered and not used (2026-10-07).** It does exactly the
tracking half, but it is a Java AAR that wants Gradle, which this APK
deliberately does not have (`docs/notes/android-build.md`), and it is
certified per phone. What it buys over writing it here is mostly per-device
calibration — and Android publishes most of that itself
(`LENS_INTRINSIC_CALIBRATION`, `LENS_POSE_*`, `SENSOR_INFO_TIMESTAMP_SOURCE`),
which the mode reads and logs. Whether the OnePlus reports them is the first
thing its log answers.

## What exists today

**Nothing the camera sees is kept**: a frame is looked at and dropped.

**Tracking is built (2026-10-08), building matching is not.** How the camera
and the motion sensors follow the hand — in plain terms and in detail, with the
bench's numbers and what it taught — is [camera-tracking.md](camera-tracking.md).
In short: steady to about a centimetre relative to where it locked, anchored on
the map once by a GNSS fix and the compass, and not yet run on a phone.

- **Camera:** the back camera, 640×480 `YUV_420_888`, plane 0 only (that *is*
  greyscale), frame timestamps from `AImage_getTimestamp`, not turned (the
  lens's orientation and intrinsics say how it stands, and turning moves the
  principal point). `platform::camera::run_back_gray`; the lens is
  `cartopolis_core::lens::Lens`, with `Lens::pinhole` scaling the intrinsics
  from the active array to the frame.
- **Motion:** accelerometer and gyroscope at 5 ms (200 Hz — Android 12+'s cap
  without `HIGH_SAMPLING_RATE_SENSORS`), **every** event kept for the reader
  (`platform::precise_sensors`, ten seconds' worth at most). The compass keeps
  only the newest; a tracker integrates, so a dropped event is a gap.
- **One clock:** `CLOCK_BOOTTIME` nanoseconds — the sensors' own base, and the
  camera's when its timestamp source is `REALTIME` (`Lens::realtime_timestamps`).

## The log, for a phone nobody here can hold

Developer tools ▸ **Precision** shows a small text log (24 KB at most, kept on
the phone across launches in `app_data_root()/high_precision.log`) with
**Copy** and **Clear**: the lens calibration as JSON when the camera opens,
then every 5 s the camera, accelerometer and gyroscope rates, the newest
frame's size and brightness, the GNSS fix and accuracy and the compass bearing
and accuracy, plus any error. It is text so it can be pasted into a chat from
the phone; frames cannot travel that way and are not in it. The matcher's
results will be added to it when it exists.

Copying on Android needed the clipboard wired for the first time
(`platform::android::copy_text`, `ClipboardManager.setPrimaryClip`) — until
2026-10-07 `utils::clipboard::set_text` was a no-op there.

**A recording of frames and sensors uploaded to the server was built first and
dropped (2026-10-07):** the camera is for positioning, not for keeping, and the
matcher can be developed against rendered views of the city from known poses
(the shot harness) with real-world confirmation on the phone after. If that
confirmation turns up something a log cannot explain, a frame recording is the
tool to bring back — as a developer switch, not in the menu.

## Raw GNSS (2026-10-07)

Under the location fix Android exposes the chip's raw measurements: a code
range per satellite and frequency (metres of noise) and the **carrier phase**
(`accumulatedDeltaRangeMeters`: millimetres of noise and an unknown whole
number of ~19 cm waves, which differencing against a nearby reference station
resolves — RTK). Whether a phone reports usable phase is a property of its
chip and driver, Snapdragon's record is mixed, and no capability flag says so:
only counting `ADR_STATE_VALID` at runtime does. So the mode **measures it
before anything is built on it.**

- `GnssRaw.java` registers a `GnssMeasurementsEvent.Callback` (a framework
  subclass, hence Java) with **full tracking** (`GnssMeasurementRequest`,
  API 31+) — with duty cycling on, the chip's clock jumps between bursts and
  every jump is a phase break, so the test would measure the power saver. It
  packs each measurement into 21 longs; `platform::gnss_raw` reads them back
  (`RawMeasurement::from_row` — the layout is shared and must change on both
  sides) and `RawStats` tallies them.
- The log gets the chip name and the platform's capability flags once (from
  API 34 these include a carrier-phase flag — a claim, which is why the counts
  below are the answer), then every 5 s a `raw ·` line: epochs/s,
  satellites per constellation, and per band (L1/E1, L5/E5a) the satellite
  count, mean C/N0, the share with a known transmit time (usable for a code
  range), the share with valid phase, the share valid *and* unbroken, slips,
  resets, and the share of valid phase with the half-cycle ambiguity resolved.
  Also clock discontinuities (should stay 0 under full tracking) and how old a
  measurement is when the app reads it.
- Nothing is solved yet and nothing is stored beyond those counts.

What the numbers would buy, from the literature: phone RTK with a base a few
km away is decimetres while walking in a benign street, 1–2 m among tall
buildings; phone code-only solutions (the Smartphone Decimeter Challenge) are
~1 m at best. With a phone's own antenna an ambiguity *fix* is intermittent;
the phase is still worth having for velocity and for smoothing the code range.

**Corrections are free here.** Kadaster/NSGI run an anonymous NTRIP caster —
`ntrip.kadaster.nl:2101` (443 with TLS), user name an e-mail address, RTCM 3.3
MSM — and one of its stations, `GRZC00NLD`, is on the Zernike Campus, 2–3 km
from the Grote Markt. Hourly and high-rate RINEX for post-processing is on
nsgi.nl. (NETPOS network RTK is for Kadaster/Rijkswaterstaat and contractors
only; 06-GPS is commercial.) Galileo HAS's free ~20 cm corrections are on E6,
which phones do not track, so they would have to be fetched and applied here.

**GNSS height is ellipsoidal (ETRS89), the terrain is NAP**: subtract the
NLGEO2018 quasi-geoid, about 41 m in Groningen (PROJ grid
`nl_nsgi_nlgeo2018.tif`; Android's `getMslAltitudeMeters` uses a global geoid
and is coarser). Over a city centre one sampled value will do.

Licences, if a solver is ported rather than written: RTKLIB and demo5 are
BSD-2; the Rust `gnss-rtk` crate is AGPL in current versions, which rules it
out for the app; `rinex` is MPL-2.0.

## Not verified

None of it has run on a device, the raw GNSS listener included: there is no
Android runtime in the development container, and the emulator cannot run this
app. The Android code compiles
(`cargo check -p cartopolis_android --target aarch64-linux-android`).

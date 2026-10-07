---
topic: high-precision mode (camera localisation)
triggers: [high_precision, HighPrecision, HighPrecisionLog, precise_sensors, run_back_gray, "lens.rs", Pinhole, PreciseHand, "camera localiser", "visual localisation", VIO, "lens intrinsics", LENS_INTRINSIC_CALIBRATION, CLOCK_BOOTTIME, copy_text, "Developer tools ▸ Precision"]
updated: 2026-10-07
---

# High-precision mode

A mode on the locate button (long-press ▸ **High precision (camera)**) that is
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

Expected: 10–30 cm absolute where buildings are in view, centimetres relative
over seconds. Netherlands only (3DBAG), daylight, buildings in frame.

**ARCore was considered and not used (2026-10-07).** It does exactly the
tracking half, but it is a Java AAR that wants Gradle, which this APK
deliberately does not have (`docs/notes/android-build.md`), and it is
certified per phone. What it buys over writing it here is mostly per-device
calibration — and Android publishes most of that itself
(`LENS_INTRINSIC_CALIBRATION`, `LENS_POSE_*`, `SENSOR_INFO_TIMESTAMP_SOURCE`),
which the mode reads and logs. Whether the OnePlus reports them is the first
thing its log answers.

## What exists today

**Nothing the camera sees is kept**, today or later: a frame is looked at and
dropped. The matcher is not built, so switched on the mode runs the sensors and
the locate button and reports on them; `PreciseHand::active` is held `false`
by `high_precision::drive_high_precision` — a mode that claimed centimetres it
does not have would make every hand-sensing script lie.

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

## Not verified

None of it has run on a device: there is no Android runtime in the development
container, and the emulator cannot run this app. The Android code compiles
(`cargo check -p cartopolis_android --target aarch64-linux-android`).

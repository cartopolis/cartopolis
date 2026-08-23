---
topic: device-orientation
triggers: [orientation.rs, locate.rs, "compass", "magnetometer", "heading", "bearing", "deviceorientation", "webkitCompassHeading", "ASensorManager", "rotation vector", "screen.orientation", "locate button", "track my position", "gps"]
updated: 2026-08-23
---

# The compass: which way the *screen* is pointing

The locate arrow in the left-hand column tracks the device — its position (GNSS)
and its heading (magnetometer). The position half already existed for driver mode
(`platform::location`); this note is about the heading half
(`platform::orientation`) and the two things about it that are easy to get wrong
in a way that looks plausible on a desk and is wrong in the hand.

## 1. Every provider reports the *device*; the user turns a *picture*

`deviceorientation.alpha`, `webkitCompassHeading` and Android's rotation vector
all describe the device's own top edge. Hold the phone in landscape and the top
edge points 90° away from what the user is looking at. A compass that skips the
correction is exactly right in portrait, 90° out in landscape and 180° out upside
down — i.e. correct in the orientation you test it in and wrong in the one people
navigate in.

The correction is `orientation::screen_up_bearing`, and the sign is the part worth
pinning:

> Portrait, top pointing north (device bearing 0). Turn the phone 90°
> **anticlockwise**: its top now points west (270), and the system turns the
> content 90° **clockwise** to keep it upright, so `screen.orientation.angle`
> reads 90. The picture has not moved — screen-up is still north. `270 + 90 ≡ 0`.

So the screen angle is **added**. Subtracting reads 180° out, and only in
landscape, which is a bug report that sounds like "the compass is unreliable".

The same quantity is `screen.orientation.angle` on the web and
`Display.getRotation() × 90` on Android — both defined as how far the *content* is
rotated clockwise inside the device. The legacy `window.orientation` (Safari before
16.4) uses the opposite sign, which is why the web arm negates it in that fallback
and not in the modern path.

## 2. Android could not use the obvious API

`SensorEventListener` is a Java **interface**, and implementing one needs a class.
When this was written the APK had `android:hasCode="false"` and no `classes.dex`
([android-build.md](android-build.md)), so the JNI route was not available at all
— not inconvenient, unavailable. The NDK's C sensor API (`ASensorManager`,
`ASensorEventQueue`) needs no dex, so the Android arm is `ndk-sys` FFI on a
dedicated thread with its own `ALooper`, and is the one Android seam that is
deliberately **not** in `platform::android` (everything in that file is JNI, and
keeping that true is what makes it readable).

**The constraint lifted on 2026-08-23** (the dex lane) and this code did not
change, deliberately: a `SensorEventListener` implementation is now possible, but
the C API already works, needs no JNI attach on the sensor thread, and is not the
seam that motivated the dex. Recorded so the next reader knows this is a choice
rather than the leftover of a limit.

Two framework facts have no C equivalent and so are in `platform::android` anyway:
`display_rotation_deg` (§1) and `package_name` (for
`ASensorManager_getInstanceForPackage`, which is how the platform attributes the
sensor's power draw).

### Sensor choice, and the one that was rejected

| Sensor | Why / why not |
|---|---|
| `ROTATION_VECTOR` (11) | **First choice.** Magnetometer fused with the gyroscope, so it is steady *while the phone is being turned* — which is the entire gesture. |
| `GEOMAGNETIC_ROTATION_VECTOR` (20) | **Fallback.** No gyroscope needed, so it exists on cheaper hardware; lags and wobbles near metal. |
| `GAME_ROTATION_VECTOR` (15) | **Rejected**, despite being the smoothest. It drops the magnetometer, so its yaw is relative to wherever the device was switched on. That is not a compass, and it fails silently — the map turns beautifully and points at nothing. |

`w` is optional in a rotation vector: it was added in API 18, and an older driver
reports three values, leaving `data[3]` as whatever the buffer held. The
quaternion is a unit one so the component is recoverable; the stale float is not,
and would read as a heading tens of degrees out with nothing to indicate it. The
Android arm checks the norm and recomputes.

## 3. What the web can and cannot do

- **Chrome/Android** fires `deviceorientationabsolute`. Its plain
  `deviceorientation` is *relative* — alpha starts wherever the page loaded — so
  believing it would point the map at an arbitrary bearing. The provider drops any
  event that is neither `absolute` nor carrying `webkitCompassHeading`.
- **Safari** has no absolute event at all, but its `deviceorientation` carries
  `webkitCompassHeading` (already a true-north bearing of the device top). Both
  listeners are registered, always.
- **iOS additionally gates the whole event** behind
  `DeviceOrientationEvent.requestPermission()`, which must be called under a user
  gesture. `OrientationWatch::start` is reached from the frame in which the locate
  button was tapped, which is inside Safari's ~5 s transient-activation window. A
  `start` reached any other way (a restored session, a script) will be refused, and
  that refusal lands in the slot as an error rather than as silence.

## 4. Design decisions in the client

**The compass writes `CameraState::follow_heading`, not the avatar's yaw.** The
avatar's facing is a smoothed function of the keys held and the camera azimuth
(`avatar::advance_facing`); feeding a compass into it would put two writers on one
field and leave the view turning after the phone had stopped. `follow_heading` is
an `Option` consumed at the single site in `free_camera` that already turned the
follow camera, so the compass is a *choice of source* and not a second code path.

**The avatar turns with the phone, not just the camera.** The follow camera sits
*behind* the avatar, so turning only the camera orbits the model — the marker for
"me" then points somewhere I am not, which is worse than not turning at all. The
same yaw goes to `AvatarState::facing_yaw`, but **only while standing still**:
`avatar::handle_avatar_movement` turns the avatar toward where it is *travelling*,
and that is the right answer whenever there is a walk input (the on-screen stick
is on the same phone the compass is). Overwriting it every frame would leave the
avatar walking sideways with the run clip playing.

**While a compass is reporting, `follow_rotate` is held rather than set.**
`free_camera` clears that flag on a rotate-drag, and a compass mode that switched
itself off the first time a thumb brushed the screen would read as broken. The
drag still turns the view; it is eased back. On a platform with **no** compass
(every desktop) the old behaviour stands — a drag drops the rotate-follow, and
that ends the tracking, because there the button *is* the old follow-and-rotate.

**The button's cycle went from three states to two** (2026-08-15, at the user's
request). The old middle rung followed the avatar without turning with it; on a
phone that is the odd one out, and needing two presses to get "put me on the map,
the right way up" is a worse default than the state it skips. That state is still
reachable from Tab and the Camera menu.

**And the button no longer picks a camera mode at all** (2026-08-21, at the user's
request). It used to force Follow on the way in and `track_device_pose` stood down
the moment the mode was anything else — which made "where am I" a one-way door out
of Fly and Earth: you pressed it to see your position and lost the view you were
in to get it. Tracking is a *position* verb, so it now runs in whichever mode is
on and moves the camera instead of the switcher. Two things follow.

The **heading half stays Avatar-only**, and not as a simplification: the
rotate-follow rig is the Follow camera's, so in Fly and Earth there is nothing for
a bearing to turn, and arming `follow_rotate` behind those modes would leave it
set, waiting to swing the view for whoever switched back. The compass is still
read and still reported in the button's caption there — what it says the device
answers does not depend on what is consuming it.

The **position half snaps once and then follows by delta**
(`locate::tracked_ground_offset`). The press is the "take me there", so the first
fix puts the camera on it — Fly keeps its altitude and its look, Earth re-centres
its ground focus, both exactly as `geo_nav::apply_teleport` already does for those
modes. Every fix after it moves the camera by however far the *fix* moved, which
keeps the map sliding under a walk while leaving a pan or a fly free to change
where you watch it from. Re-centring every frame was the obvious alternative and
is worse: it is a camera the user cannot move while the button is lit, with
nothing on screen to say why.

**Driver mode wins.** It owns the avatar's position, the camera rig and its own
pair of watches; `locate::track_device_pose` stands down whenever
`DriverState::active`, so exactly one system writes the position at any moment.

## Verification status

**Neither device arm has run on hardware from this container.** The maths is unit
tested (`orientation::tests`, `locate::tests`) and the container has no
magnetometer, no GNSS and no display rotation to read. What a phone still has to
confirm:

- that the screen-angle correction has the sign argued for in §1 — turn the phone
  into landscape and check the map does *not* swing 90°;
- that `display_rotation_deg` reports the activity's display and not the physical
  panel;
- that a device with only `GEOMAGNETIC_ROTATION_VECTOR` reaches the fallback
  rather than the "no compass" error;
- on iOS, that the permission prompt appears from the button tap.

The headless Android emulator on the VPS (see the container's own notes) can run
the APK but reports no magnetometer, so it can confirm the *error* path and the
absence of a crash, not the heading.

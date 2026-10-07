---
topic: device-orientation
triggers: [orientation.rs, locate.rs, "compass", "magnetometer", "heading", "bearing", "deviceorientation", "webkitCompassHeading", "ASensorManager", "rotation vector", "screen.orientation", "locate button", "track my position", "gps", "6dof", "first person", "point the phone at"]
updated: 2026-10-07
---

# The compass: which way the *screen* is pointing

The locate arrow in the left-hand column tracks the device — its position (GNSS)
and its heading (magnetometer). The position half already existed for driver mode
(`platform::location`); this note is about the heading half
(`platform::orientation`) and the two things about it that are easy to get wrong
in a way that looks plausible on a desk and is wrong in the hand.

**Since 2026-09-17 the seam carries a whole rotation, not a bearing** — see §5,
which is where "point the phone at a building and see that building" lives. The
bearing is still there and is still what the map arrow reads;
it is derived from the rotation now (`DeviceAttitude::bearing_deg`) rather than
reported beside it.

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

**In third person the compass turns nothing** (2026-10-07, at the user's
request). It used to: `locate` wrote the bearing into a `follow_heading` the
chase rig eased its azimuth toward, held `follow_rotate` on against drags, and
turned the avatar with it. On a phone in the hand that read as the camera
stuttering and moving irregularly — magnetometer noise, and the bearing's lurch
whenever the phone tipped, swung the whole boom about under a walk. So
`follow_heading` and its faster `COMPASS_ROTATE_RATE` were removed rather than
damped: the third-person view stays where the user put it, and the compass is
read in third person only for the button's caption. Rotating the view with the
phone is a first-person thing (§5).

In first person the avatar's facing follows the *view*
(`locate::facing_from_view`), so the body faces where you were looking when you
zoom back out — but **only while standing still**: `avatar::handle_avatar_movement`
turns the avatar toward where it is *travelling*, and that is the right answer
whenever there is a walk input (the on-screen stick is on the same phone the
compass is). A test pins `facing_from_view` against the compass convention for a
level phone, because a sign slip faces the avatar plausibly and wrongly.

On a platform with **no** compass (every desktop) the button is still the old
follow-and-rotate: arriving in Follow arms `follow_rotate`, which turns with the
avatar's own facing, a drag drops it, and dropping it ends the tracking.

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

The **heading half stays first-person-in-Follow only**: Fly and Earth hold views
the user placed by hand, and third person has a boom the phone cannot aim (above).
The compass is still read and still reported in the button's caption everywhere —
what it says the device answers does not depend on what is consuming it.

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

## 5. Aiming the camera with the phone (first person)

Turning a map under a level phone and *pointing* a phone at a building are two
different requests of the same sensors, and the second one needed three things the
first did not.

**The seam carries the screen's whole attitude.** A bearing is one number and the
view needs the tilt as well — and, worse, the old number is degenerate in exactly
the pose the new question is asked in: held upright, screen-up *is* the vertical,
and the compass bearing of a vertical vector is noise. `DeviceAttitude` is
therefore the rotation from the screen frame into the sensors' East/North/Up, and
`bearing_deg` is derived from it. Both device arms already had the whole thing and
were throwing it away: Android's rotation vector is a quaternion, and the web's
`alpha`/`beta`/`gamma` are a full ZXY Euler triple of which only `alpha` was read.

**The screen frame is Bevy's camera frame**, which is why
`DeviceAttitude::camera_rotation` is one change of world basis and nothing else. A
camera looks down `−z` with `+y` up; a phone's back camera looks down `−z` with
`+y` up the screen. So the only thing that can be wrong is the ENU → world
quarter-turn, and one test pins it on all three axes. Deriving Euler angles and
re-composing them was the alternative and is worse twice over: it has a gimbal
pole a phone reaches by being pointed at the sky, and it drops the roll.

**It is gated to first person, and to the locate button.** Three conditions
(`locate::device_aims_the_view` plus the button, which is that function's caller):

| Condition | Why it is not optional |
|---|---|
| First person | In third person there is a boom between the eye and the avatar. Tipping the phone up swings that boom *under the pavement* to look up past the model — the view moves, but not to what the phone is aimed at. There is no third-person answer to this request. |
| Follow (Avatar) mode | The boom is the Follow camera's rig; Fly and Earth hold views the user placed by hand, and a sensor is not entitled to take one. |
| The locate button lit | Aiming a view by the magnetometer while the app has no idea where the user is standing puts the right *direction* at the wrong *place*. Position and attitude are separate sensors here (§ the module note) and the pairing is what makes a building on screen the building in front of them. |

**Getting in is the zoom, and there is one threshold.** `first_person_after_zoom`
turns a gesture that asked to come closer than `AVATAR_CAM_MIN` into the step
inside, and any outward gesture into the step back out. The distance stays parked
at the minimum throughout, so both comparisons are against that one number —
a separate exit threshold would be a second constant to keep in step with the
first, and they would disagree the first time either moved. Crossing it must not
swing the view, which is a real constraint and not a nicety: the first-person
rotation is built from the same `azimuth`/`pitch` the boom was, and
`stepping_inside_keeps_looking_the_same_way` pins that they produce the same
direction.

**A drag cannot fight this one.** In third person a rotate-drag simply turns the
view, since nothing else is turning it (§4). Here the view *is* the phone's
attitude, so a drag could only push the picture off the thing the phone is pointed
at and be dragged back the same frame. Zooming out is how the camera is taken
back.

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
- on iOS, that the permission prompt appears from the button tap;
- **the whole of §5 on a real phone**: zoom into first person with the locate
  button lit, hold the phone up, point it at a building and check that the
  building is on screen — then turn on the spot and check it stays put. Both
  device arms feed it and neither has been in a hand.

On iOS specifically, §5's yaw is *reconstructed*: Safari has no absolute `alpha`,
so the arm turns `webkitCompassHeading` back into the `alpha` that would have
produced it and builds the rotation from that plus the (absolute, gravity-derived)
`beta`/`gamma`. That is exactly what this seam already assumed of every Safari
reading when it carried only a bearing, so it is not a new risk — but it is the
one platform where the attitude is not a fused quaternion out of the sensor stack,
and an upright phone is the pose in which that substitution is least well founded.

The headless Android emulator on the VPS (see the container's own notes) can run
the APK but reports no magnetometer, so it can confirm the *error* path and the
absence of a crash, not the heading.

---
topic: camera tracking (visual-inertial odometry)
triggers: [cartopolis_vio, "crates/vio", vio, odometry.rs, attitude.rs, scale.rs, "image.rs", sim.rs, precise_tracker, Tracker, Anchor, "Lucas-Kanade", "Lucas–Kanade", FAST, "visual-inertial", "gyro bias", "camera tracking", HAND_HEIGHT_M]
updated: 2026-10-17
---

# Camera tracking

How the high-precision mode follows the phone in a hand to the centimetre
(`crates/vio`, `cartopolis_vio`; run by `systems::precise_tracker`). The mode
around it — the switch, the sensors, the log — is
[high-precision-mode.md](high-precision-mode.md); what uses the hand is
[hands-and-tones.md](hands-and-tones.md).

## In plain terms

The camera is the eyes and the motion sensors the inner ear; neither is good
enough alone.

1. **Landmarks.** Thirty times a second the camera takes a small greyscale
   picture and picks out ~150 sharp details: window corners, brick joints, the
   edge of a sign.
2. **Following them.** In the next picture it finds the same details again. Move
   the phone right and they slide left; move closer and they spread apart; near
   ones shift more than far ones — which is how distance is judged.
3. **The gyroscope says how it turned**, very accurately. With the turning known,
   whatever the landmarks still did must be the phone moving.
4. **The accelerometer says how big the move was.** A camera alone cannot tell a
   small step past a near wall from a big step past a far one; the accelerometer
   feels every push and stop, and comparing the two over a few seconds gives
   real centimetres.
5. **On the map once.** At the lock it takes one GNSS fix and the compass
   heading and pins the tracking there. That is metres off, and for a virtual
   theremin nobody can see it does not matter: what matters is the hand moving
   steadily relative to it.

To use it: switch it on and **hold the phone still** a moment (it learns which
way is down and how its gyroscope drifts), **move it a little** (the landmarks
need parallax, the scale needs motion), and it says **locked**.

## The pipeline

**Frames.** *Device*: Android's sensor axes (x right, y up, z out of the screen,
portrait). *Camera*: the raw image (x right, y down, z along the lens). *World*:
z up, heading wherever the phone pointed at the start. The back camera's
mounting comes from `SENSOR_ORIENTATION` alone (`Camera::back`: a half turn about
x, then a quarter turn about device z per 90°); `LENS_POSE_*` is ignored because
the one phone measured reports it relative to itself.

- **`image`** — FAST-9 corners on the 640×480 Y plane, at most one per 32 px cell,
  topped up when fewer than 120 of a target 160 survive. Followed by pyramidal
  Lucas–Kanade (4 levels, 9×9 window) and kept only if tracking back lands
  within a pixel of the start.
- **`attitude`** — the gyroscope integrated. "Up" is set from the accelerometer
  averaged over a fifth of a second of stillness, and the gyroscope's bias is
  that stillness's mean gyro reading, refined whenever the phone is still for
  half a second again. Tilt is then pulled towards gravity gently (0.05/s) and
  only on readings within 3 % of one g. Heading is never touched.
- **`odometry`** — positions up to scale, the rotation taken as known. Starting:
  once ~2° of parallax and 20 corners in common, the baseline is the smallest
  eigenvector of `Σ(a×b)(a×b)ᵀ` over world bearings; landmarks are triangulated.
  Each frame after: the camera centre solves `Σ(I−bbᵀ)c = Σ(I−bbᵀ)L` (3×3), with
  landmarks whose ray misses by over 3 px dropped. A Gauss–Newton step then
  solves position and a small rotation together against the landmarks; a tenth
  of that rotation goes back to the attitude, and the position is solved again
  under exactly what was applied. New corners become landmarks at ~1.5° of
  parallax. Fewer than 8 landmarks is a restart, and a restart is a new scale.
- **`scale`** — metres per visual unit. Over the last 4 s, `s·p_v(t) = p₀ + v₀τ +
  ½bτ² + D(τ)` at every frame, `D` the gravity-free acceleration integrated twice;
  ten unknowns by least squares, the bias `b` held to ~0.05 m/s² by a prior.
  Windows with under 4 cm of motion or a fit worse than 3 cm RMS are skipped;
  good ones are blended, weighted by motion, forgetting 2 % a frame.
- **`precise_tracker`** — the tracker on its own thread, fed every motion
  reading and only the newest frame. `Anchor` places its world on the map at the
  lock: GNSS fix, compass yaw (from where the back camera looks, which needs the
  phone not pointed at the ground), and `HAND_HEIGHT_M` = 1.3 m. The fix is read
  once — following it would hand the hand its metre of jitter. A restart of the
  same tracker continues from the last hand with the same heading; a new tracker
  (the camera was taken away) re-reads the compass.

## Measured

The bench is why `cartopolis_vio` is Bevy-free, for the reason `cartopolis_geo`
is: the tracker is developed and tested off the phone, against synthetic frames
and motion with a known answer, and none of that should wait on the client's
link.

**On the bench, 2026-10-08** (`crates/vio/src/sim.rs`): points on a façade 6 m
ahead, a pavement and a side wall; the phone held upright at 1.4 m, still for a
second, then swaying ±25/12/10 cm with the wrist turning ±6–10°. Motion readings
at 240 Hz derived from the path, frames at 30 Hz.

| Case | Error |
|------|-------|
| Perfect sensors, corners projected exactly | worst 1.2 cm |
| Gyro bias 0.002 rad/s, accel bias 0.04 m/s², noise, 0.5 px corner noise | median 6.6 mm, 95th 11.7 mm |
| As above, through rendered 640×480 frames and the whole image pipeline | **median 4.9 mm, 95th 9.9 mm** |

Error is measured after aligning the tracker's world to the truth at the first
tracked frame (it has its own heading and origin). The rendered case took 3.1 s
in the suite on 2026-10-27 (a Mac node), and 1.7 s once `sim::render` drew its
rows on threads; the drawing is most of it.

**From the first phone log (OnePlus 15, 2026-10-07):** intrinsics reported
(2795 px on a 4096×3072 array: ~437 px, ~72° across at 640×480), no distortion,
`LENS_POSE_REFERENCE` 0, sensor orientation 90°, timestamps `REALTIME` — the
camera and the gyroscope share a clock. Frames arrive 80–107 ms after they were
taken; the camera ran at 30 fps and the motion sensors at ~240 Hz with a steady
4 ms gap.

## What the bench taught

Each is a constant in the code with this beside it:

- **Tilt from the accelerometer must be gentle.** At 0.5/s the orientation read
  2–4° wrong with perfect sensors — a swaying hand is not gravity — and a degree
  is a decimetre of position at the façade's distance.
- **The gyroscope's bias must be measured**, and only in sustained stillness. A
  phone's few thousandths of a rad/s tilted the estimate a degree in ten
  seconds; re-measuring at every pause read the sway's own turning points as
  bias and tripled the error.
- **The landmarks may correct the rotation only slowly.** With most landmarks at
  one depth a 2 cm shift and a 0.2° turn look alike; taking the whole correction
  each frame traded one for the other and tripled the error on perfect data. A
  tenth per frame removes drift without that, provided the position is solved
  again under the rotation actually applied.
- **The scale window must outlast the slowest sway**, and the bias must be a
  prior. A 2.5 s window and a free bias read a 2.9 s sway 2–3 % short.

## Not done, and what lost

- **Not run on a phone.** The bench says the maths holds; it says nothing about
  motion blur, rolling shutter, a phone's real bias drift or its CPU.
- **No prediction to now.** The pose is the newest frame's, ~0.1 s old; the
  gyroscope could carry it forward and does not yet.
- **No lens distortion model.** The phone reported none (all zeros).
- **Building matching is not built** — pinning the anchor tighter than GNSS by
  matching the camera against the 3DBAG models. Published results put it at
  metres in cities, and a theremin needs relative steadiness, so it came second.
- **ARCore** would do this half, and was not used: a Java AAR needing Gradle,
  which this APK deliberately lacks, and certified per phone.
- Weak where any visual tracker is: the dark, blank walls, fast motion.

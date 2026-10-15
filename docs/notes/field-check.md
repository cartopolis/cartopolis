---
topic: field-check
triggers: [field check, field_check.rs, FieldPose, field-checks.toml, open_still, view_angles, "see_picture", "photo beside the render", roll, fov, on_ground]
updated: 2026-10-15
---

# The field check: a photograph and the world from the same pose

A harness for the agent first (`docs/mockups/field-check/`). In a direct chat
with the agent account, a camera button in the chat bar opens a capture screen;
one shutter makes one picture, the photograph on the left and Cartopolis on the
right, cropped to 3:4, with the pose written under them, and sends it as an
encrypted chat picture. The pose also rides in the caption as one marked line
of JSON (`cartopolis_core::field_check`), so it needed no protocol change; the
chat shows only the note, and `read_chat` hands the agent the pose as a ready
`[[shot]]` block for `docs/field-checks.toml`.

## Facts that are not obvious from the code

- **The harness's yaw turns the other way from a compass.** `--look`'s yaw is
  counter-clockwise seen from above (90 looks west); a compass heading is the
  negation (`FieldPose::harness_yaw_deg`). `ShotSpec`'s comment said
  "clockwise" until 2026-10-15, which was wrong. Roll is the third angle of the
  same `EulerRot::YXZ` the free camera is built from, read off the phone with
  the same decomposition (`DeviceAttitude::view_angles`), so it needs no sign
  convention of its own.
- **A field check's `fov` is the vertical angle of the 3:4 half**, not of the
  phone's sensor or its screen. A re-render has to be made at a 3:4 size
  (`MANIFEST_HEADER` says 750×1000) or the same angle frames a different
  picture. On the phone the world camera is given the wider angle that makes the
  window's 3:4 crop see exactly that (`window_fov_y_deg`).
- **`at`'s height is above the ground underfoot only with `on_ground = true`.**
  Without it a preset's altitude is above the *map anchor's* ground, which with
  the terrain layer on is metres off at another spot: an eye-height camera ends
  up underground or in the air.
- **The lens comes from the focal length and the sensor size**, because the
  OnePlus 15 publishes no calibration (`oneplus-15` memory, 2026-10-08). The
  fallback when a phone publishes neither is 72° along the long side.
- **The compass accuracy is the rotation vector's own `values[4]`** (radians,
  −1 for none); the event's accuracy *class* is not an angle and is not used.
- **The world camera is teleported only to arrive and after 200 m of walking**;
  every smaller step moves the camera directly, so a walk is not a stream of
  teleports or of map re-streams.
- **Only one camera can be open.** The high-precision mode holds the back
  camera; a field check while it is on reports the camera as taken.
- **The height is data only.** `ground_ahn_m` is the app's height map under the
  camera; the receiver's altitude is above its own coarse geoid and is the
  antenna's, so comparing them needs the Dutch geoid (NLGEO2018) and the
  antenna height, neither of which is applied.

## Not verified

Built and checked on the desktop, Android (compiled for aarch64) and web
targets on 2026-10-15; **none of it has run on a phone**. The Camera2 still
stream (largest 4:3 YUV size up to 1920 wide), the compass accuracy, the pose
driving the world camera, and the alignment of the two halves all wait for a
first field check on the device.

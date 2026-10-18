---
topic: retired/field-check
triggers: ["field check", field_check.rs, FieldPose, field-checks.toml]
updated: 2026-10-18
---

# Retired: the field check

Removed 2026-10-18. Its Mixed picture is now one of the camera's three
modes, and its pose is in the EXIF of every picture the camera makes
(`camera.md`, `systems::camera_screen`, `cartopolis_core::photo_pose`). The
aerial panel was not carried over: the agent renders the aerial from the
pose when it wants it. What follows is the note as it stood.

## The field check: a photograph and the world from the same pose

A harness for the agent first (`docs/mockups/field-check/`). **Developer tools ▸
View ▸ Field check** opens a capture screen; one shutter makes one picture of
three panels (the photograph and Cartopolis, cropped to 3:4, and the aerial
photograph from above with the camera and its line of sight marked), with the
pose and the address written under them. It is
shown on screen; **Send to viberfox** sends it as an encrypted chat picture to
the direct chat with the first account in `CARTO_FIELD_CHECK_WITH`, and **Save
to phone** writes it as a JPEG into the gallery (`Pictures/Cartopolis`; a
desktop's `~/Pictures/Cartopolis`). The pose also rides in the caption as one marked line
of JSON (`cartopolis_core::field_check`), so it needed no protocol change; the
chat shows only the note, and `read_chat` hands the agent the pose as a ready
`[[shot]]` block for `docs/field-checks.toml`.

## Facts that are not obvious from the code

- **It was a camera button in the chat bar** of a chat with the agent until
  2026-10-18, and nobody found it there. It replaced Developer tools' "Capture
  slot N", which saved the view into `docs/img/screenshot-N.png`: nothing linked
  those files, and on a phone that path is nowhere.
- **The aerial panel is the map from above** (`aerial_reference::from_above`,
  `--aerial-reference`'s `<shot>.aerial-map.jpg`): camera a red dot, line of
  sight red to where the view meets the ground, at most 150 m ahead. The
  camera-matched aerial (`from_camera`, `<shot>.aerial.jpg`) was tried first and
  rejected on 2026-10-18: from 3.5 m on Grote Markt it was a grey smear of
  paving under a sky, with every building flattened into it, while the map from
  above showed the square, the trees and which way the camera faced. The map's
  shape follows the line of sight, so it is fitted whole into its panel, not
  cropped. Its tiles are fetched on the composing thread ("Fetching the aerial
  photo…"); outside PDOK's envelope or on a failed fetch the panel says so and
  the picture is still made.
- **A saved picture carries its pose in EXIF** (`FieldPose::exif`, hand-written
  TIFF, since `image` writes an EXIF block but builds none): GPS position,
  direction, horizontal error, UTC date and time, an altitude when the height map
  had the ground, and the caption's pose line as the UserComment, so
  `FieldPose::from_caption` re-reads a saved file as it reads a chat picture.
  **The direction is tagged magnetic** (`GPSImgDirectionRef = M`): Android's
  rotation vector is referenced to magnetic north and no declination is applied
  (about 2° in the Netherlands); a pose from the app's own view is true north.
- **The address is looked up after the shutter**, on the thread that puts the
  picture together, from the same Nominatim as the saved places'
  (`bookmarks::short_address`). A failed lookup leaves `place` empty and the
  picture is still made; the place is also the preset's `description`.
- **The capture screen names what it is waiting for**: the GPS (or why there is
  none, e.g. the permission was refused), then the compass, and the shutter stays
  grey until both have answered.
- **Three panels are 1584 px wide**, under the 1600 px a chat picture is
  re-encoded to. A re-render for one panel is still 3:4 (`MANIFEST_HEADER`'s
  750×1000): only the angle matters, not the pixel count.

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

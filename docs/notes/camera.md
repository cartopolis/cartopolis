---
topic: camera
triggers: [camera_screen.rs, photo_pose, PhotoPose, jpeg_meta, world_polaroid, "In world", Mixed, selfie, EXIF, XMP, ImagePick, open_still, upright_turn, view_angles, photos.toml, "To phone"]
updated: 2026-10-18
---

# The camera: three modes, and the pose in every picture

The dock's orange disc, Camera in a chat's + menu and in the post composer
open one camera (`systems::camera_screen`, designed in
`docs/mockups/social-camera/`): **In world** (a polaroid of the view),
**Mixed** (the phone's photograph beside Cartopolis drawn from the phone's own
pose) and **Real** (the photograph alone), each with a selfie flip and an
upload button. It replaced the field check on 2026-10-18, a developer tool that
made the Mixed picture (plus an aerial panel) and nothing else
(`retired/field-check.md`).

## Facts that are not obvious from the code

- **Every picture carries its pose in the file**: EXIF for what EXIF has tags
  for (position, error, altitude, direction, time, Orientation = 1, a 35 mm
  focal length) and an XMP packet under the app's own namespace
  (`https://cartopolis.org/ns/pose/1.0/`) for heading, pitch, roll, vertical
  angle, shape and height. Not a drone's or a photogrammetry tool's namespace:
  their angles count from other zeros (a DJI gimbal pitch is −90 straight down),
  and a reader trusting the name would read ours wrong. The UserComment still
  holds the whole pose as the caption's JSON line, which reads back with
  `PhotoPose::from_caption`.
- **A picture's EXIF survives being sent or posted** (`utils::jpeg_meta`, called
  by `photo::prepare`): it used to be dropped by every re-encode. The
  orientation is reset to 1 because `prepare` turns the pixels. An upload keeps
  the EXIF it came with, GPS included; on Android the system picker may leave
  the GPS out of what it hands an app.
- **A public post keeps the full pose too** (the user, 2026-10-18), not only the
  position the post shows: where you stood to the centimetre with RTK, the
  height and when, readable by anyone who saves the picture.
- **The agent reads the pose from the caption**, not the file: a picture sent to
  a chat also carries the pose line in its caption (`cartopolis:pose …`), and
  `read_chat` hands it over as `pose`, with a `[[shot]]` block for
  `docs/photos.toml`. Captions from field checks (`cartopolis:field-check …`)
  still read, as 3:4.
- **The direction is magnetic** for a pose from the sensors
  (`GPSImgDirectionRef = M`): Android's rotation vector is referenced to
  magnetic north and no declination is applied (about 2° in the Netherlands). A
  pose from the app's own view (In world, or a desktop) is true north.
- **`aspect` says what shape `fov_y_deg` is the vertical angle of.** Mixed and
  Real pictures have the photograph's shape, which follows how the phone was
  held: 4:3 sideways, 3:4 upright. In world is the polaroid's 960:900. The shot
  manifest has one `size` for the run, so a picture taken sideways needs a run
  of its own at its shape (`MANIFEST_HEADER`).
- **The still camera turns with the phone** (`camera::upright_turn`, the
  display's rotation read twice a second on the camera thread). A call's camera
  does not: it still assumes the phone is upright.
- **A photograph without lens data is given a main camera's 72° along its long
  side**, because the OnePlus 15 publishes no calibration (`oneplus-15` memory,
  2026-10-08). The long side is whichever the phone was held along.
- **In Mixed the world camera is teleported only to arrive and after 200 m of
  walking**; every smaller step moves the camera directly, so a walk is not a
  stream of teleports or of map re-streams. Leaving Mixed puts the view back.
- **The harness's yaw turns the other way from a compass**
  (`PhotoPose::harness_yaw_deg`): `--look`'s yaw is counter-clockwise seen from
  above; a compass heading is the negation.
- **`at`'s height is above the ground underfoot only with `on_ground = true`**;
  without it a preset's altitude is above the map anchor's ground, which with the
  terrain layer on is metres off elsewhere.
- **Only one camera can be open.** The high-precision mode holds the back
  camera; Mixed or Real while it is on reports the camera as taken.
- **Upload on Android is the system picker** (`ImagePick.java`): its answer
  arrives in `MainActivity.onActivityResult`, which `android-activity` does not
  surface to Rust, so the bytes are read in Java and polled. The camera app
  (`ACTION_IMAGE_CAPTURE`) is no longer offered for chat pictures: Camera is the
  app's own camera.
- **The web build has no camera** (`platform::camera` is not compiled there):
  the dock shows no camera disc, and a post's picture is an upload.

## Not verified

Built and checked on the desktop on 2026-10-18; **none of it has run on a
phone yet**: the turn with the display, the picker's answer, the selfie's
heading, and the alignment of the two halves of a Mixed picture held sideways.

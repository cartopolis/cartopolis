---
topic: avatar-faces
triggers: [avatar_face.rs, face_tracking.rs, avatar_video.rs, crates/face, crates/tract_tflite, crates/human/src/face.rs, face_units, FACE_UNITS, Expression, ExpressionState, SetFaceFrame, FaceFrameOf, MorphWeights, assets/models/face, CARTO_FACE, faceunits01]
updated: 2026-10-27
---

# The avatar's face moves: expressions, blinks, and the user's own face

Three ways an avatar's face moves, all through the same 52 weights
(`cartopolis_human::face::UNITS`, Apple's ARKit blend shapes, sorted):

- **An expression** the user picks (Shift+1…8, `Expression`), held for
  `EXPRESSION_SECS` and replicated like an emote: an edge on `ClientIntent`,
  held with a sequence on `AvatarStateDto`. Each is a recipe of units
  (`face::recipe`), composed by hand from FACS descriptions.
- **A blink** every 2–6 s, on every client, never on the wire.
- **The user's own face**, read from the front camera by MediaPipe's Face
  Landmarker (`crates/face`), while the camera's In world selfie is up or a
  call sends the avatar. Sent as `SetFaceFrame` (52 bytes, at the camera's
  15 fps) and relayed by the sim to clients within `SAY_RADIUS_M` on the voice
  lane, kept nowhere. **Only the weights leave the phone, never a picture.**

## The units are morph targets on a face part of its own

The units are the `faceunits01` asset pack (CC0, Mika Suominen): ordinary
`.target` files on the MakeHuman base mesh, in the pack as `face_units` at a
finer step (0.05 mm, `FACE_UNIT_STEP`; a turned eye moves millimetres).
`build` turns each into per-vertex position and normal changes, and the client
hands them to Bevy as morph targets weighted by the holder's `MorphWeights`.

**The engine stores a target's change for every vertex of a mesh**, 48 bytes
each (`MorphAttributes`: three padded `Vec3`s), and caps a mesh at 2048² × 9
floats. The whole body (~14 000 drawn vertices × 52) is over that cap; so the
skin the units move — 3 166 of the 13 380 skin vertices, measured 2026-10-27 —
is drawn as a part of its own, `face`, the only skin part with targets. Its
normals are summed over the whole skin before the split, so the seam does not
shade. Eyes, brows and lashes get targets through their fitting rule (the
units move the helper vertices the eyes hang on); hair does not.

## Face tracking: MediaPipe's models, run by tract

`assets/models/face/*.tflite` are `face_landmarker.task` unzipped (Apache
2.0): BlazeFace short range (128²), the landmarker (256², 478 points), and the
blend-shape model (146 of the points → 52 scores, `_neutral` first, no
`tongueOut`). The graph's calculators — anchors, box decoding, the 1.5×
square levelled on the eyes, the crop, the points back into the picture, the
146-point subset (`kLandmarksSubsetIdxs`, copied from
`face_blendshapes_graph.cc`) — are done in `crates/face` from MediaPipe's
documented behaviour. The detector runs only until a face is found; the next
square comes from the last points.

**tract-tflite 0.23.9 could not run them**, so `crates/tract_tflite` is it,
patched (each change marked "Patched for cartopolis"):
- DEQUANTIZE (float16 weights → float32), PRELU, NEG, SQUARED_DIFFERENCE added;
- a tensor with no shape read as a scalar, where it panicked;
- negative reduce axes counted from the end;
- STRIDED_SLICE with `shrink_axis_mask`, which tract supports, allowed;
- **a float mean multiplied, not `quant::scale`d: that op rounds to whole
  numbers**, so the blend-shape model's centring was off by up to half a
  pixel and every score came out 0 or 1 (a smile read 1.00 / 0.00). Found by
  comparing every tensor with `ai-edge-litert`'s interpreter on the same
  points; after it, tract matches to 0.01.

Measured 2026-10-27 on mac-2 (M5 Pro VM, release): first frame 13 ms with the
detector, a followed frame 11 ms. The phone is unmeasured. Tracking runs on
the camera's own thread, inside its sink, so the frame loop never waits.

## The avatar in a call

"Show your avatar instead" (a call's More sheet) films the local avatar with a
second camera (`avatar_video`): a child of the body's holder, 0.75 m before
the head joint at rest, rendering 512 × 384 (rows of exactly 2048 bytes, so
the read-back has no padding), read back at 15 fps and sent as the call's
camera pictures. Ordinary video, sealed like any: the other side needs nothing
new. The device camera meanwhile is the tracker's.

- **Its far plane culls nothing**: the city behind the avatar is drawn (a
  4 m far plane still showed buildings 30 m off, 2026-10-27), so it is a
  second, small view of the whole scene. Its cost on a phone is unmeasured.
- **egui draws onto every Core3d view**, and the window's scissors on a
  512 × 384 target failed the whole submission — headless, the run just never
  finished its capture. `egui_pass` skips cameras ordered below 0; the avatar
  camera is -1. A new offscreen camera must be ordered below 0 too.

`CARTO_AVATAR_VIDEO=/tmp/x.png` films the avatar with no call and writes the
pictures there, for a capture.

## Captures

`CARTO_FACE=laugh` or unit weights (`CARTO_FACE=jawOpen=1,eyeBlinkLeft=0.5`)
holds every face still at that, for a `--shot-mode avatar` capture.

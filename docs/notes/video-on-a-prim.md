---
topic: video on a prim
triggers: [prim_video.rs, "video on a prim", "prim video", SetPrimVideo, PrimVideo, "/api/videos", WebM, webm, matroska, ClipPlayer, prim_video, play_video]
updated: 2026-09-30
---

# Video on a prim

A prim can play a clip on its surface, for everybody near it, in step. Wire
protocol 28 (`SetPrimVideo`, `PrimDto::video`), migration V22 (`prims.video`,
the `videos` table), `simulator::videos`, `cartopolis_calls::clip` and the
client's `systems::prim_video`.

## What is decided and why

- **Same decoders as calls.** `cartopolis_calls::clip` demuxes WebM with
  `matroska-demuxer` (pure Rust, no dependencies) and decodes with the VP8 and
  Opus decoders a call already uses (`calls.md`). So the formats are **WebM,
  VP8 picture, optional Opus sound** and nothing else: no H.264 for the same
  patent reason as calls, and no second decoder stack to keep.
- **The server checks the container, never a frame.** Upload
  (`POST /api/videos`) refuses anything that is not WebM with one VP8 track
  within `cartopolis_core::video_limits` (64 MB, 1280×720, 10 minutes, the
  duration measured from the last frame rather than the header) and at most one
  Opus track. Decoding is every viewer's job, and the decoder has its own pixel
  ceiling for a file whose header lies.
- **A store of its own, not the model store.** Clips are named by content hash
  like models, but the model store's tables are about glTF — triangles,
  licences, a library — and a clip has none of that.
- **In step by a start time, not a stream.** The server stamps
  `started_at_ms` when a clip is set; each client seeks to
  `now − started_at_ms` (modulo length when looped) from the nearest keyframe.
  Nothing is streamed live; every viewer fetches the file once and caches it by
  hash. Clock skew between devices shows as that much offset, which is fine for
  a screen in a square.
- **Its own message, never part of the save.** `UpdatePrim` is a full-state
  edit that knows nothing of video, so a colour change must not stop a clip
  (the integration test pins it). Starting and stopping is `SetPrimVideo`,
  gated like every other edit: owner or admin.
- **What bounds it is the upload to the GPU, not the decode.** A picture goes
  to the GPU whole each time it changes — the documented way this client loses
  a phone's GPU (`upload-bursts.md`). So only prims within 60 m play, at most 4
  at once (1 on Android), nearest first; pictures are scaled to 640 px wide
  (320 on Android) on the decoder thread; and every picture goes through
  `stream_ledger`, the per-frame upload budget, which drops what it refuses.
- **Drawn unlit.** A screen gives light; it does not take the sun's. Stopping
  (or walking out of range) hands the prim back to its own texture.
- **Only a prim's own shape is a screen.** A glTF model has no single material
  to paint, so a prim wearing a model does not play.
- **Not in the web build.** The browser version would decode with the
  browser's own `<video>`; that is the same piece of work as web calls and is
  not built.

## How to drive it

- In the viewer: edit a prim you own ▸ Video ▸ **Choose video…** (desktop) or
  **Stop**.
- From an agent: the MCP tool `play_video {id, file | asset, looped}`; neither
  `file` nor `asset` stops it.
- Make a clip: `ffmpeg -i in.mp4 -c:v libvpx -b:v 1M -c:a libopus -vf scale=640:-2 out.webm`.

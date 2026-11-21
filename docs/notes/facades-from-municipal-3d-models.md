---
topic: facades
triggers: [crates/atlas/src/pipeline/facades.rs, model_roofs.rs, mapillary.rs, street_photo.rs, facade_edits.rs, facade_colours.rs, facade_check.rs, building_windows.rs, building_windows.wesl, formats/facades.rs, "facade colour", "city model", "3dgroningen", Mapillary, "scene network", "facade network"]
updated: 2026-11-20
---

# Facades: what a wall is made of, from photographs nobody shows

3DBAG gives a building's shape and roof and nothing of its walls. A wall's
colour, material, window lattice and roof covering come from photographs —
the municipality's oblique ones, Mapillary's street ones, or one a player
takes — and **no photograph is ever drawn on a wall**. The owner decided that
(2026-11-19): the shape is derived from the photos, the photos stay out. A
texture-page attempt was built and reverted for it.

## The city model (`pipeline/facades.rs`, `model_roofs.rs`)

Groningen publishes a textured 3D city model as 3D Tiles:
`https://www.3dgroningen.nl/datasource-data/open/5801f042-5f31-4bfb-af91-a63e79e925e7/tileset.json`
(gzip). Its GLB leaves carry `EXT_structural_metadata` naming each part
`id_bag_<pand>`, so walls match BAG ids directly. z18 leaves are about 10 cm a
texel. The host answers 502 to more than two requests at once (`MAX_FETCHES`).
**Licence not confirmed**: the portal states none for it; ask
opendata@groningen.nl before anything measured from it ships beyond Groningen
testing.

A wall is rectified from its leaf texture and judged by a facade network:
SegFormer-B0 trained on CMP facades (Xpitfire's, MIT), exported to ONNX at
512² in, 128² out, run with `tract-onnx` (`crates/atlas/models/facade_segformer_b0.onnx`,
LFS). On oblique aerial photos it tells only facade, window and background
apart. Rejected first: Marco333's CMP model, which labels everything wall.

What a wall must pass before its colour counts, and why each test exists:

- **Line of sight** against the 3DBAG heights: an oblique texture holds other
  buildings' roofs where the camera could not see the wall.
- **Windows in storeys, 8–40 % glass** (`MIN_WINDOW_SHARE`): a dormered roof
  textured as a wall passed every colour test before this.
- **Its own shade test** (`SHADE_LUMA`), and colour measured against the white
  of its own facing (`Light`, `WHITE_ALBEDO` 0.8), dehazed where that facing
  has no white. Rejected: median and k-means (brick came out grey), the leaf's
  brightest white (Hooghoudt went white), a white dimmer than the wall
  (255,248,214).
- **Prior from the registers**: year, OSM `building:material` and
  `building:colour`, rijksmonument. A material is a vote over clearly-brick
  texels weighed against it.

**What it cannot do**: a north-facing wall is never sunlit in the flight and
keeps the era palette; window frames and fine detail are below 10 cm. Some
walls come out the wrong colour outright — the hotel on the Grote Markt
(pand 0014100022192356, Hooghoudt on its roof) read grey-brown, it is red
brick. That is what the corrections below are for.

Roof coverings (`Covering`) come from the best-lit face. Solar panels only
where a 1 × 1.6 m lattice repeats (`panel_grid`); noise never reaches 0.5.

## Corrections: Mapillary and players' photos

The building card has **Facade wrong** (measure it again from Mapillary) and
**Facade photo** (send a photo of it). Atlas measures, files the result in
its own stamped tree `facade_edits/` (`crate::facade_edits`) — never in
`facades/`, which the next dataset run rewrites — and logs who, when and what
was there before in `facade_edits.jsonl` in its data directory. The client
lays that tree over `facades/` and polls its manifest like building designs.
`DELETE /api/pipeline/buildings/{pand}/facade` (building-override operators)
takes one out.

### Placing the camera

A Mapillary photo has a computed pose; a phone's has a fix 5–10 m out and a
compass that is worse beside steel shopfronts, so it gets none. Atlas
**searches** for the pose: it draws the 3DBAG shells around the building from
every place within the fix's error, every heading towards the building, every
tilt and roll, and keeps the drawing that agrees best with what the photo
shows — building, sky and street (`street_photo::search`). Mapillary poses are
checked the same way and moved a few metres (`refine`).

**The facade network cannot judge a street photo.** Trained on crops of one
facade, it calls the street, trees and sky facade too, and the search then
fills the frame with buildings (tried 2026-11-19: every pose wrong). A scene
network does it: UperNet with ConvNeXt-T, ADE20K (`openmmlab/upernet-convnext-tiny`,
**MIT**), exported at 576² in, 144² out, half precision, 119 MB
(`crates/atlas/models/scene_upernet_convnext_tiny_fp16.onnx`, LFS). Too big for the
binary: a deployed atlas reads it from `<data_dir>/models/`. Rejected for
licence: NVIDIA's SegFormer ADE/Cityscapes weights and Mask2Former's
(non-commercial / unstated).

Measured on four Mapillary photos used as phone photos, fix 7 m out, no
compass (2026-11-19): two poses within 3–5 m and 2–4°, one 12 m out with the
right heading, one on the neighbour until roll joined the coarse search.
The fit score does not tell a right pose from a neighbour's (both 0.4–0.75);
it only rejects no-pose-at-all (`MIN_FIT`).

### Why one photo is not enough

A phone sets exposure on the whole scene, so a wall in shade reads dark: the
same brick came out 173,138,109 / 130,112,97 / 91,77,69 from three photos.
Reading it against its own white frames helps only where they are in view.
So a measurement pools several photos with equal shares (`SHARE`): the
player's and up to eight of Mapillary's. Pooled, repeat runs agree within
about 5 per channel (130–135,113–120,102–110; the hotel 85–87,61–62,58).
One run takes 70–110 s on an M5 core set and **5 min 13 s on the VPS**
(2026-11-20, load ~10), so a request only starts it: `POST …/recheck` and
`…/photo` answer 202 at once, the app asks `GET …/facade/status` every 5 s,
and one measurement runs at a time.

**The scene network is stored in half precision and run in full**
(`FloatPrecisionTranslator` at load). Run in half, an x86 host emulates every
operation: one photograph took 25 minutes on the VPS against ten seconds on
an M5.

Mapillary's API answers a bbox with an arbitrary 500 of the thousands there,
different each time, so it is asked in 60 m tiles (`TILE_M`). Views are spread
at least 6 m apart, recent ones (three years) before old ones — a building
photographed in its scaffolding is not the building — and wide action-camera
lenses (focal < 0.55) are left out: the drawing has no lens distortion.

**Licence**: Mapillary imagery is CC BY-SA 4.0; a correction from it is
marked `s: mapillary` and credited in the data layers panel. The token is
`ATLAS_MAPILLARY_TOKEN` in atlas's environment, never in code. A player's
photo is read and dropped; atlas keeps nothing of it.

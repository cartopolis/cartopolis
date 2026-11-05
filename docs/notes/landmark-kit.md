---
topic: landmarks rebuilt from measurements and photographs — the kit
triggers: [crates/atlas/src/landmark, crates/atlas/landmarks, kit.rs, gather.rs, score.rs, glb.rs, "atlas landmark", Martinitoren, landmark, "building model"]
updated: 2026-11-05
---

# Landmarks: the kit

3DBAG draws a landmark the way it draws an office: one reconstructed shell,
and the client paints storeys of office windows on it. The Martinitoren came
out as a grey 97 m block (2026-11-05, before/after in the commit). The
Euroborg was fixed with a generator written for stadiums alone
(`building-models.md`); a generator per building does not scale, so landmarks
are written as **kits**, files a builder (an agent or a person) writes from
measurements and photographs, which one compiler turns into the GLB the
override store already takes. Deleting the override still restores the shell.

## The loop

```bash
atlas landmark gather <pand> --name Martinitoren --out dir   # facts.md, photos.md, aerial.jpg, kit.json
atlas landmark build crates/atlas/landmarks/martinitoren.json --out dir
                                    # model.glb, placement.json, score.md, lod3-<x>-<y>.json
```

`facts.md` holds the outline in a local frame squared to the building, the
laser surface at a metre, a histogram of heights (a tall bar is a ledge) and
**extent by height**. `photos.md` lists Commons photographs within 300 m whose
titles name the building, with the bearing each looks along, so you know
which face it shows. Every photo used is credited in the kit's `sources`.

`score.md` compares the model with AHN's surface model from above. **Read its
plan-area-by-height table, not the pixel match.** On a 15 m tower half-metre
edge effects keep the pixel match near 35% for a model that is right; the area
ratio says which stage is too wide or too short. The Martinitoren's first
fit was 15–30% too wide at every stage top because the stages taper
(buttress set-backs). Fitted widths to the mid-stage area plus a set-back ring
brought every height within ~10%, with a mean height error of −0.3 m.

## Traps

- **Extent by height is a bounding box.** Pinnacles and galleries inflate
  it. Size a stage from the area in the score instead (a square of side √area;
  an octagon of circumradius √(area / 2√2)).
- **AHN does not see a spire.** The Martinitoren's laser top is 93 m and its
  published height is 96.8 m. Take the height from the record.
- **Photographs are scaled to the laser, not to themselves.** Even at 159 m
  the photograph's vertical scale drifts with height. Fix three ledges to
  their laser heights and read everything in between.
- **Openings sit on vertical stretches only.** An opening on a tapering
  stretch is refused, so a tapering stage is a vertical stretch holding its
  arches plus a short set-back above it.
- **Openings on one face must not overlap.** Clocks the tower hangs inside an
  arch are drawn centred between two arches instead. That is a named
  simplification in the kit's sources.
- **The AHN6 sheet index answered "Given layer does not exist"** on
  2026-11-05 (`ahn6.rs`'s `INDEX`), so AHN6 falls back to AHN4 for any sheet
  not already cached. The kit tools use PDOK's AHN4 directly.

## Previewing without publishing

The lod3 file `build` writes is what the store would serve. Serve it from a
stand-in host on the node that renders, passing every other path through to
`3dbag.cartopolis.org`, and point the client at it with `CARTO_3DBAG_URL`.
The file is placed by the outline's first vertex, while the store asks the
published shells (`models::owner_cell`), so check the shell cell names the
pand.

## Limits

One flat colour per part and material, no textures (`building-models.md`).
Parts are vertical lofts, so a thin disc facing sideways or a sloped roof
plane must be cut as an opening or built from lofts. Solid parts are named
`… wall` and need the record's collision set to `walls`, so the panels behind
openings stay walk-through.

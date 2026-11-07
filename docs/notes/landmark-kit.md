---
topic: landmarks rebuilt from measurements and photographs — the kit
triggers: [crates/atlas/src/landmark, model_assets.rs, replace_building, crates/atlas/landmarks, kit.rs, gather.rs, score.rs, glb.rs, "atlas landmark", Martinitoren, landmark, "building model", measure.rs, aerial.rs, photo.rs, "photo-pose", "landmark section"]
updated: 2026-11-06
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

## Measuring between the steps (2026-11-06)

```bash
atlas landmark section <kit|pand> --heading 90 --through x,y --out dir   # section.md/.png
atlas landmark aerial  <kit|pand> --cm-per-px 5 --out dir                # aerial.png/.md
atlas landmark photo-zoom    --photo p.jpg --at u,v --out dir            # zoom.png
atlas landmark photo-pose    <kit> --photo p.jpg --point u,v=x,y,z … [--from lat,lng] --out dir
atlas landmark photo-measure --pose pose.json --wall x1,y1:x2,y2 --px u,v …
atlas landmark photo-rectify --pose pose.json --photo p.jpg --wall … --top 40 --out dir
atlas landmark photo-overlay <kit> --pose pose.json --photo p.jpg --out dir
```

All in the kit's local frame, so a number read off one goes into the kit as
it is. A pand instead of a kit uses the frame `gather` gives it.

- **`section`** cuts the laser (and the kit, red) along a line: steps,
  levels, a table every half metre and a drawing at 12 px a metre. Steps a
  half metre apart are joined into one: the laser smears a tall wall over
  several pixels, and the Martinitoren's flank read as thirty steps before.
- **`aerial`** is PDOK's orthophoto turned into the local frame at a stated
  scale, with a metre grid, the outline, the laser's contours (about ten over
  the building's height) and the kit's plan. **PDOK answers a box whose shape
  differs from the picture's with a blank white JPEG and a 200**, so the box
  is made square. The orthophoto is not true-ortho: a tower's top leans off
  its foot, so read a plan from the contours, not a roof's edge.
- **`score.md` gains "Walls against the laser"**: for each solid wall, how far
  to move it outward, and one line for the shift of the whole kit that best
  explains them. The Martinitoren: every main wall within 0.6 m, and the kit
  standing 0.4 m off the laser along its own +y (east-north-east); the Forum
  0.25 m, the Groninger Museum 0.55 m. Only `solid` parts
  count, and walls under a metre wide: an opening's panel or a pinnacle's face
  is beyond the half-metre laser and was told to move 3 m.
- **Photographs.** `photo-pose` solves where a photograph was taken from four
  or more points whose pixel and local position you know (corners at the
  laser's heights, from `facts.md` and `section`). It refuses a camera nobody
  could have held — under the ground, kilometres off, a lens past 20 picture
  widths — and names the closest fit refused: points that disagree otherwise
  solve to a camera 30 km off at 0.2° across, where everything lines up as on
  a drawing. `photos.md` gives each photograph's own coordinate for `--from`,
  a start for the search and not a constraint (the uploader's, often tens of
  metres off). Then `photo-measure` reads lengths on a wall, `photo-rectify`
  straightens one into an elevation at a stated scale, and `photo-overlay`
  draws the kit's visible edges over the photograph with a fit figure that
  counts both ways (photo edges met, drawn edges landing on one).
- **Place points with `photo-zoom`.** On the whole 1280 px thumbnail a
  ledge's corner is a smudge: the first real try, Commons'
  `20140621 Martinitoren en Mutua Fides…`, with five corners placed by eye,
  solved to 38 px RMS and an overlay that missed the tower. Zoomed 8× the
  same corner sat two pixels off where it had been placed. **Not yet done:
  a real photograph solved to a few pixels** — the solver, measuring,
  straightening and overlay are proven on synthetic photographs only.

## Parts whose faces lean their own way

A part is either a `shape` scaled through `rings`, or `layers`: the plan at
each height. Layers of unequal point counts are resampled by angle round the
first layer's centre (`samples`), so each must be star-shaped about it.
`gather` writes the plan at five heights as convex hulls, which is what the
Forum's layers are (2026-11-05: within 5% of the laser's area to 44 m). A
hull bridges a courtyard, so the Forum's sunken roof terrace is not drawn.
Openings sit on any face, leaning or not: width along the bottom edge,
`from`/`to` as heights.

The nearby-photo search matches titles, which fails for a name like "Forum":
it found fridge magnets. A Commons category (`Category:Groninger Forum`) is
the better source then.

## Traps

- **Extent by height is a bounding box.** Pinnacles and galleries inflate
  it. Size a stage from the area in the score instead (a square of side √area;
  an octagon of circumradius √(area / 2√2)).
- **AHN does not see a spire.** The Martinitoren's laser top is 93 m and its
  published height is 96.8 m. Take the height from the record.
- **Photographs are scaled to the laser, not to themselves.** Even at 159 m
  the photograph's vertical scale drifts with height. Fix three ledges to
  their laser heights and read everything in between.
- **An opening must fit one stretch between two rings.** One across a step
  is refused.
- **Openings on one face must not overlap**, nor a window row a glass band. Clocks the tower hangs inside an
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

## Publishing

The app's `replace_building` tool (`systems::dev::mcp::agent`, over the
Midas device link too) makes the Overrides tab's two requests as the
signed-in account: the GLB to `/api/pipeline/overrides/{pand}/model`, then
the name, placement (`placement.json` from `build`), `walls` collision and no
interior. `restore_building` deletes it. The account needs
`building_override`, and an agent holds no operator login of its own, so a
landmark goes live through a person's signed-in app.

## Limits

One flat colour per part and material, no textures (`building-models.md`).
A material may name a `finish` (`formats::lod22::FINISHES`): `slab` for
stone cladding, `clear` for see-through glass (the Forum's railings). Clear
glass is a 40 % screen door on the models' inside material, not blending:
the cell is one merged mesh and a blended one would need sorting. A kit's
window material is named `glass` and stays opaque, which is why the finish
is not called that.
A sideways disc or a tilted plate is built from `layers` (a thin plan
shifted between two heights is a leaning plate, as on the Museum). Solid parts are named
`… wall` and need the record's collision set to `walls`, so the panels behind
openings stay walk-through.

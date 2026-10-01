---
topic: roof-colours-from-aerial-imagery
triggers: [pipeline/roofs.rs, formats/roofs.rs, "roof colour", "roof texture", "building texture", "facade texture", orthoHR, ortho25, "solar panels", "lean", "true ortho", ATLAS_ROOFS_DEBUG]
updated: 2026-09-30
---

# Roof colours are measured from the aerial photo, in atlas

Asked 2026-09-30: can the buildings get realistic textures from public Dutch
sources? What was settled, and why.

## Facades: no public source

Nothing open covers Dutch facades nationally. Street-level imagery is either
licensed against this use (Google Street View, Cyclomedia), patchy (Mapillary,
CC-BY-SA) or one city (Amsterdam's own panoramas). Oblique aerials are not
public. Projecting photos onto LoD2.2 walls is research anyway
(`3d-basisvoorziening.md`: "enriching LoD2 with facade openings from oblique
imagery is current research"). Walls stay generated from the registers (era,
use, storeys, doors).

## Roofs: colour and material, never the photo itself

Draping the aerial photo on roofs was rejected before it was built: a 25 cm
pixel is a whole roof tile, so a photo roof beside a crisp wall reads as blur;
the photo's shadows and sun disagree with the engine's; and the photo half of a
building next to an engine-lit half looks like two games. What the photo *can*
give is the fact a generated palette gets wrong — red clay here, anthracite
there, black bitumen behind. So atlas measures a colour and a material per
building (flat planes and pitched planes separately), and the client draws a
material texture tinted to it. It runs in atlas because the per-building cost
(a few hundred photo fetches per cell, rasterising every roof) is done once
per city rather than on every phone.

## Measurements (Groningen centre, cells 8490/5319–5320, 2026-09-30)

- **Layer.** `Actueel_ortho25` (summer) and `Actueel_orthoHR` (8 cm, early
  spring) were both run through the pipeline. The 25 cm layer is colour-dead in
  the warm direction — confirmed again here after `aerial-imagery-cannot-give-
  paving-colour` found it for paving: its red tiles measure brownish pink,
  barely apart from grey concrete ones. orthoHR separates them plainly. orthoHR
  is the default; `ATLAS_ROOFS_LAYER` switches.
- **Colour cast.** The mosaic is not neutral: grey roofs measured ~(118, 125,
  116) on ortho25, which classified a fifth of all flat roofs as sedum until the
  cast was taken out against the median flat-roof pixel of the same cell. Gains
  came out the same in both cells (to 0.01), so the reference is stable.
  orthoHR's cast is under 1 %.
- **Lean.** The mosaic is not a true orthophoto. Unmoved, a 20 m block's mask
  sat ~3 m off its roof, half on its own facade. Measured per 370 m block from
  its tall (≥ 12 m) flat roofs, as the shift that makes the roof mask cover the
  most uniform patch of photo: the two cells lean in opposite directions (a
  flight-line seam between them), 0.1–0.3 m per metre of height. Fits at the
  edge of the search are discarded; a block with fewer than five fits takes the
  cell's median.
- **Throughput.** ~22 s per cell, dominated by the WMS fetch (16 blocks of
  1600² per cell, three at a time). 7,700–7,900 buildings per cell, 99 %
  measured.

## Solar panels: tried and dropped

At 0.23 m per pixel a panel field, a slate roof and a bitumen roof reflecting
the sky are the same dark blue-grey. A colour test flagged 7,211 roof surfaces
in two cells, and a grid-contrast test (mean absolute Laplacian over the
panel-coloured pixels) gave a known slate roof 19–22 against a median of ~20
for everything flagged — no separation. The grid is below the pixel. Doing it
properly means sampling at the native 8 cm (an 18,400 px cell) or a separate
detector; until then a dark roof gets its measured colour, which is right
whichever it is.

## Haze, and why walls cannot be measured here

- **Haze (2026-09-30).** The darkest half-percent of orthoHR over Groningen is
  (31–33, 42–45, 53–54), bluish, not black: the scattered-light veil of a photo
  taken from altitude. It is subtracted per channel in linear light
  (dark-object subtraction) before any measurement; it measured the same in
  both cells.
- **Wall colours: tried and removed.** The idea was that the lean which moves a
  roof off its footprint also shows the facade facing the camera — the one
  facing *against* the lean — as a strip beside the roof. Implemented and
  measured over the same two cells: the strips are one or two pixels wide even
  for three-storey blocks, and requiring a 6 px band left a few hundred
  buildings whose samples, painted back onto the photo, are still the roof's
  edge and the shadow under it. The mosaic is cut from the most vertical part
  of each exposure, which is what makes it a good roof source and a useless
  facade one. Facade colour stays generated from the building's era.

## Paving colour, and why brick is chosen rather than measured (2026-10-01)

Atlas's `roofs` step also measures every paved BGT surface in the cell it has
the photo for, keyed by the surface's first vertex (`formats::roofs::paving_key`):
roofs masked out, the photo's sun and shade split at Otsu's threshold, a
sunlit surface measured from its sunlit pixels and an alley brought back out of
the shade by the cell's own per-channel sun/shade ratio (1.8–2.7× in two
Groningen cells). ~60 % of surfaces get a colour; the rest keep the material's.

`aerial-imagery-cannot-give-paving-colour` (2026-09-10) still holds for what it
asked — one red brick against another — and the measurement confirms its other
half: colour survives in *direction* only. The Grote Markt, red brick to anyone
standing on it, measured (161, 151, 144): red minus green of 10, against a
median of 3 over the cell's 644 brick surfaces and ~0 for every other material.
So a brick surface's measurement picks red / yellow / anthracite / buff and the
kind supplies the colour at the measured lightness (`roofs::brick_colour`); other
materials keep their measured colour. Herestraat came out yellow, the Grote
Markt red, the Vismarkt grey pavers with a yellow strip, matching the 2026 5 cm
photo.

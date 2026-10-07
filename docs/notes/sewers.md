---
topic: sewers
triggers: [sewers.rs, GWSW, riolering, KLIC, "beheerstedelijkwater", maaiveldhoogte, bob, GROUND_SEE_THROUGH, see_through, GroundKey, "underground", "cables and pipes", "pipes in the ground"]
updated: 2026-10-07
---

# What is under the street, and what we may draw of it

## Only the sewers are open

**KLIC** (Kadaster) holds every cable and pipe in the country — gas, water,
power, telecom, district heating, sewers — because the WIBON obliges every
operator to register them. It is handed out per request (a dig notification,
or a paid orientation request) for one area, and its terms forbid
republishing it; some operators also withhold detail for security. So it can
never be an atlas dataset streamed to every player. The one shape it could
take here is a person loading *their own* KLIC delivery onto their own device.

**The sewers are the exception.** Municipalities publish them in the GWSW
vocabulary (Stichting RIONED), and PDOK serves the lot, **CC0**, as
`service.pdok.nl/rioned/beheerstedelijkwater/wfs/v1_0` (types `BeheerLeiding`,
`BeheerPut`, `AansluitingLeiding`, …). About 117 municipalities and 15 water
boards were in it in 2026; Groningen was the 2021 pilot. There is no OGC API
Features endpoint for it (two plausible `api.pdok.nl` paths 404'd).

Also open, and not used yet: the high-pressure **transport pipelines** (Gasunie
gas, chemicals) on the risk map — regional lines, not street mains.

## What the register holds (Groningen, measured 2026-10-07)

Over RD 228000–240000 × 577000–588000: **35,229 pipe sections** (34,878 the
city's own), 29,432 with a bottom level at the start. Types: 16,121
`GemengdRiool`, 5,921 `Hemelwaterriool`, 4,257 `Vuilwaterriool`, 3,623
`Persleiding`, 3,265 `Drain`, 784 `Duiker`, then a tail.
`AansluitingLeiding` (house connections) is **empty** here. The city alone is
40,521 pipes and 30,398 manholes.

Each pipe carries width/height (mm), material, shape and the bottom-inside
level (*bob*) at both ends; each manhole a surveyed ground level
(`maaiveldhoogte`) and shaft height (mm).

## Depth, not level — and the datum question

**Ground minus invert** (the manhole's ground level minus the pipe end's bob,
joined on the pipe's `uriB`/`uriE`) over 147 pipe ends in the centre: 1.23 m
at the 5th percentile, **1.72 m median**, 3.13 m at the 95th. Atlas writes
that depth and the client hangs it under its own terrain, so the datum both
levels are in never matters.

Not settled: whether those levels are NAP. Manhole ground levels in the centre
read 4.2–7.9 m. An AHN comparison was tried and abandoned (the WCS answers
only deflate-predicted GeoTIFF, which nothing in the container decodes without
writing a reader). Check it before anything consumes an absolute level.

## The service

Pages at **1,000** features whatever `count` asks for; pages stably only with
`sortBy`; `resultType=hits` answers `numberMatched="0"` (broken), so the walk
stops at a short page. A 4 km box is ~1.6 MB per 1,000 pipes and ~0.6 s.

## Seeing through the ground

The ground is a stack of opaque sheets ordered by a depth ladder
(`the-ground-stack.md`), so alpha blending it is out: the sheets would sort
against each other. It is a **screen door** — `ground_detail.wgsl`'s
`GROUND_SEE_THROUGH` variant discards an ordered-dither share of pixels near
the camera, the same pixels on every sheet. Two consequences:

* It is a **shader variant** (`ground_detail::GroundKey`, on both the ground and
  the road material), not a uniform: a `discard` anywhere in a fragment shader
  costs early depth testing for every draw using it, taken or not, and the
  ground is several sheets deep.
* The `discard` is at the **end** of the fragment, after every derivative and
  texture sample — before them it would put them in non-uniform control flow.

Without something under it the holes show sky, so a dark unlit sheet
(`sewers::Backdrop`) follows the camera 14 m below the ground under it.

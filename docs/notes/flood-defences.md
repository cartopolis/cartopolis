---
topic: map
triggers: [dikes.rs, dikes.py, waterkering, IMWA, dike, dijk, "flood defence",
  "sea wall", street_labels.rs, WorldLabelLine, crest, terrain.rs, height.rs,
  "Lauwersmeerdijk", waterschap]
updated: 2026-08-29
---

# The dikes were already drawn; nothing said so

This is a country that mostly lies below the sea it holds out, and the map could
not say it. What was missing was never the *shape*. A dike is bare earth, so it
is in the height model `systems::height` already fetches, and `systems::terrain`
renders it as a ridge like any other ground. What was missing is that the ridge
has a name, a type and a job.

cartopy's `dikes.ts` reads the water boards' own registers — republished together by
PDOK as IMWA — and `systems::dikes` streams them and hands the named ones to
`systems::street_labels`.

## The register, measured

`waterkering` over Groningen, Amsterdam, Utrecht and the Rotterdam delta,
2026-08-29 — 1,320 defences:

| `typewaterkering` | count |
|---|---|
| Dijk | 936 |
| *absent* | 244 |
| Kunstwerk (a lock or pumping station in the line) | 107 |
| Hoge grond | 29 |
| Dam | 3 |
| Duin | 1 |

`categoriewaterkering` is filled on every one (809 regional, 347 primary, 164
other) and `statusobject` is `gerealiseerd` on every one. There are 353 distinct
names and only two that look like internal codes.

Two things follow. **An untyped defence is kept**, because it still has a
category and usually a name — dropping one in five over a missing field would
lose real sea wall. And **the name is often not a dike's name**: a defence is
frequently called after the water it holds back (`Winschoterdiep`) or the
structure it runs through (`Sluis De Groeve Zuid`). That is what the register
says, so that is what is shown.

Coverage varies by city in a way worth knowing before hunting a bug: the
Groningen *city* box yields 150 defences of which **none** are typed `Dijk` —
they are the canal embankments, mostly untyped. The dikes proper are on the
coast: the box out to the Lauwersmeer returns 110 of them.

## Lines are split at cell boundaries, not binned by their first vertex

Every other pipeline here puts a shape in the cell owning its first point and
lets that cell draw it whole (`surveyed.rs` says so about the paving). That works
because a fence is metres long. A defence is kilometres long and crosses many
cells, so the same rule would hand a 10 km line to one cell and show it only
while that cell happened to be loaded.

So `split_by_cell` cuts each line where the cell changes and **repeats the
crossing vertex in both pieces**, which is what makes them join with no gap. Each
piece therefore overhangs its own cell by at most one segment — ~18 m at these
lines' vertex spacing, and nothing downstream cares.

## The crest has to be measured, and the terrain field cannot supply it

`TerrainField` is one 6.4 km box at 25 m per sample. Its own module note says
that is the resolution "that shows dunes, dikes-as-slopes and river valleys, and
deliberately not the one that shows a kerb", which is the right trade for the
ground the whole map stands on.

It is not enough to put a *label* on a crest. Measured on the Lauwersmeerdijk
(53.31220, 6.22390), same 400 m box, 2026-08-29:

| sample spacing | relief across the box |
|---|---|
| 0.5 m (the model's own grid) | **6.34 m** |
| 25 m (what the terrain field uses) | **4.24 m** |

So the coarse field keeps about two thirds of a sea dike and flattens its top by
roughly two metres. A name laid on that answer is buried inside the embankment it
names.

`measure_dike_crests` therefore asks for **one fine box per named defence** —
60 m across at half-metre samples, which is the model's own grid — and takes the
**highest** finite sample in it. Highest, not median: the median lands on the
slope and the mean is dragged down by the toe on both sides. Sixty metres is wide
enough to contain a big sea dike's whole cross-section, so the answer is the
crest wherever the register's reference line happens to run — that line is
usually on the crest and the register does not promise it.

`HeightLayer` keys and caches those boxes, so re-asking is free and a defence
looked at once is answered from memory. A line that is asked about and never
answered is marked settled, so it stops being asked.

**This is deliberately not a finer terrain field.** Sharpening the grid along a
line would put a second resolution inside a lookup that every mesh build calls
thousands of times, in order to change ground the camera already reads as a
ridge. The label is the one consumer that genuinely cannot use the coarse answer,
so the label is the one thing that gets a measurement of its own — carried as
`WorldLabelLine::ground_m`, which is `None` for every street and keeps the coarse
field's answer there.

The crest is metres above NAP and the label frame is metres relative to the
anchor's ground, so the conversion is a subtraction against
`HeightLayer::ground_m` — and it is skipped entirely until that datum exists,
because a label placed against a missing one is off by the anchor's own
elevation.

**With the Height layer off, nothing is measured and nothing is wrong**: the
world is flat, `TerrainField::at` returns 0, and the names lie on it. That is why
`--dump-state` carries `flood_defences` and `flood_defences_measured` as two
numbers — the first at zero means the register never arrived, and the second at
zero with the first non-zero means the names are standing on the coarse field.

## What is not drawn

No new geometry at all. A defence's embankment is ground the height model already
supplies, and adding a mesh over it would be a second, disagreeing description of
the same slope — the z-fight the paving's depth rung exists to avoid, but this
time between two answers to "where is the ground". The `Kunstwerk` defences (a
lock, a pumping station) are streamed and typed rather than drawn for the related
reason: they are genuinely part of the line and genuinely not embankments, and
there is no form here that is honestly them.

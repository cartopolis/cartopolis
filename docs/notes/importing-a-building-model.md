---
topic: buildings
triggers: [ifc_lod3.py, lod3.rs, IFC, BIM, ifcopenshell, LoD3, interior, hollow]
updated: 2026-08-28
---

# Importing an IFC model as a building

The national survey gives a *shell*: every roof plane, correct to a few
centimetres, and solid. A building has an inside, and a survey flown from an
aeroplane never will. `tools/ifc_lod3.py` converts an IFC building model into the
same cell format `lod22` already streams, and `systems::lod3` swaps it in for that
building's shell.

## What made it possible was the id

An IFC file says nothing about which national register entry it is. Matching a
model to a mass by geometry means matching against a building that may have been
extended, re-surveyed or rebuilt since either was captured — and the failure is
silent, because a wrong match still draws a building.

So the model is keyed on the BAG identificatie, given once on the command line.
That is the entire reason the LoD2.2 extractor was fixed to carry a real one:
`replaced_pands` turns "which shells do I skip?" into a set membership test.

## Georeferencing is an argument, not a discovery

IFC4 can carry an `IfcMapConversion` or an `IfcSite` latitude and longitude. In
practice a model handed over by an architect very often carries neither, or
carries a project origin in a national grid 150 km away. Guessing wrongly puts a
building in the wrong street with nothing to say it did, so the importer asks:
`--at` is where the model's origin goes and `--rotation` turns its +y away from
north. `--from-ifc-site` reads the file's own coordinates for the models that
have them — noting that IFC stores those as (degrees, minutes, seconds,
millionths) with the sign on the *first* component only, so summing the parts
without that sign puts a model on the wrong side of the equator.

## `use-world-coords`, and how it fails

`ifcopenshell.geom.settings()` returns element geometry in each element's **own**
frame, handing the placement over separately in `shape.transformation`. Applying
nothing puts the whole building at the project origin.

It is worth knowing exactly what that looks like, because it does not look like a
failure: the right number of elements, the right classes, the right triangle
count, two storeys reported — and every wall of every floor stacked on the same
spot inside a bounding box a few centimetres across. Nothing raises. The only
thing that caught it was measuring the output extent against the model's known
7 × 12 m footprint. `settings.set("use-world-coords", True)`.

## Two things follow from a model having an inside

- **Its surfaces are drawn double-sided.** Backface culling is right for a shell —
  half the triangles face away and nothing can ever be behind them — and wrong for
  a building you can stand inside, where the wall in front of you shows you its
  back. The models get their own material; the shells keep their culling.
- **Collision comes from the walls, not the footprint.** A shell's collider is the
  convex hull of the whole building, which is what keeps the avatar outside it.
  Hulling a model the same way would seal it, including sealing the doorway the
  model actually has. So `lod3::is_solid_class` gives each wall and column its own
  hull and everything else none — a floor slab's hull is the whole storey, which
  would fill the building with an invisible block.

That is the "hollow buildings" question answered in the only way that is not a
trick: not by turning culling off on a shell that has no interior surfaces, but by
importing geometry that has them.

## Testing it without a model

There is no IFC in this repository and there cannot be — a building model belongs
to whoever surveyed it. `tools/ifc_demo_building.py` writes one: two storeys,
exterior walls, an interior partition, floor slabs, a roof, and a gap in the
ground-floor front wall. The gap is the point of the fixture. A sealed box would
exercise the importer and prove nothing about whether the wall colliders leave a
doorway open.

## What is not done

- **Storeys are carried and unused.** Each element records its storey index, which
  is what a cutaway or a floor-by-floor view would key off. Nothing reads it yet.
- **No level of detail within a model.** A shell is a few dozen triangles and a
  real IFC is hundreds of thousands; a city of imported models would need the
  distance switch this does not have. `KEEP_CLASSES` is the blunt version — it
  drops the furniture, ducting and reinforcement that a map does not want, which
  is most of a real model by element count.
- **Openings are geometry, not semantics.** An `IfcDoor` is imported as a solid
  and excluded from collision, so you walk through the door leaf rather than
  around it. Correct enough, and not the same as understanding a door.

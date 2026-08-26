---
topic: canal-moorings
triggers: [moorings.rs, "boats", "canal", "moored", "water polygons", furniture.rs,
  "waterway", signed_area, "which side is the water"]
updated: 2026-08-26
---

# Boats moored along the canal banks

`cartopolis_geo::moorings` (2026-08-26). A Dutch city canal is lined with boats,
and a canal with none reads as a drainage ditch. This is the water's equivalent
of `docs/notes/parked-vehicles.md`: the polygon is surveyed, the boats on it are
inferred, and the same rule applies — be right about what the data says and claim
nothing else.

No request and no extra decode. `map_geometry` already parses the water polygons
to tessellate the surface; this walks their **boundary** and puts a boat every so
often just inside it, into the same `SurfaceGroup::Furniture` mesh and material
and under the same toggle.

## A canal is told from open water by its shape, not by a tag

The tile has no width and no `waterway` value on a polygon. What it has is
geometry, and `area / perimeter` is the mean half-width of a long thin shape — a
few metres for a canal, a hundred for a lake. `MAX_CANAL_HALF_WIDTH_M` (14 m) is
where one stops being the other.

The failure this prevents is not subtle: without it, every kilometre of the
Wadden Sea's shoreline gets a row of moored boats.

Interior rings subtract from the area and add to the perimeter, which is what
makes the ratio the *shape's* half-width rather than its outline's.

## Which side is the water depends on the winding, and MVT does not promise one

For a ring with positive signed area the interior is to the **left** of each
edge, i.e. `(-uz, ux)`. This shipped with the opposite sign first and moored every
boat 2.6 m up the quay.

It was caught before any capture because
`boats_moor_inside_the_water_whichever_way_the_ring_winds` feeds the **same ring
in both directions** and asserts the boats land inside either way. A test that
only used the winding the fixture happens to have would have passed, and a city
full of boats parked on the pavement would have shipped. Any future geometry that
needs an inward normal owes the same two-direction test.

## The tile seam is not a bank

A canal crossing a cell edge gets a straight ring edge along the seam from the
clip. Boats laid along that run down the *middle* of the water. `on_tile_border`
drops those edges — the same guard the vegetation inset makes, restated here
because that one is private to the scatter and is about a canopy overhanging a
quay rather than about a bank.

## The hull straddles the waterline

`furniture::form::BOAT` is 9 m by 2.4 m with a low cabin aft of centre, measured
off the small *woonboten* and sloops a Groningen canal actually carries — a barge
on a 12 m canal reads as a ship in a bathtub. The hull sits 20 cm below y = 0 and
the rest is freeboard, which makes it the only form in that module with anything
under the ground plane. That is deliberate: y = 0 is where the water surface is.

Hulls are dark (near-black, bottle green, deep blue, oxblood, varnished timber),
which is both what Dutch canal boats are and what the pale cartographic palette
needs — see `parked-vehicles.md` on why a bright hull would be the loudest thing
on the water.

## Not solid

`prop_radius` answers 0 for a boat. A collider there would be an invisible wall
in the middle of a canal the avatar cannot walk on anyway — the same judgement
the playground token gets, for a different reason.

## Verified

A capture over the Lopende Diep (2026-08-26) shows boats on both banks, in the
water rather than on the quay, aligned with the bank, irregularly spaced by the
vacancy hash and varied in colour. `MOORING_VACANCY` is 0.55 because a boat at
every pitch is a fence rather than a canal.

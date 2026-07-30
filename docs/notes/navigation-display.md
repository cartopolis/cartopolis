---
topic: navigation
triggers: [driver.rs, route_ahead.rs, "route rail", "speed limit", maxspeed, "driver mode", "POIs along the route"]
updated: 2026-07-30
---

# The navigation display

Standing facts about driver mode's chrome and the data behind it. What each
element *is* lives in `driver.rs` and `route_ahead.rs`; this is the part that
would otherwise be re-litigated.

## The rail's scale is not linear, and it can't be

The route rail runs the vehicle at the foot to the destination at the top. A
linear metres-to-pixels mapping is the obvious choice and it fails at exactly the
distances driving happens over: on a 40 km route, everything inside the next
2 km — which is all a driver can act on — lands inside the bottom 2 % of a 236 px
track, one unreadable clump, while the empty motorway middle gets nine tenths of
the pixels. So the near field gets a disproportionate share
(`driver::RAIL_NEAR_SPAN_M` / `RAIL_NEAR_FRACTION`) and the rest is compressed.
Both reference navigation views do the same thing, which is what suggested it.

The break is drawn as a notch across the track rather than left implicit. A scale
that changes silently halfway up is a lie about distance, and distance is the
only thing being read off it.

## Measured: what a city corridor actually returns

**2026-07-30, Groningen, 4.8 km car route (53.2194,6.5665 → 53.2350,6.5300), one
Overpass corridor query.** The first cut asked for `amenity~fuel|charging_station|
parking` at a 150 m corridor and got **45 amenities, 43 of them parking** — the
rail rendered as an identical column of `P` badges with a "+38", and the single
charging point on the route was pushed off it entirely.

Two fixes, both needed: the query now only counts a car park that is *named*, a
structure, or a park-and-ride (an unnamed `amenity=parking` rectangle behind a
terrace is not somewhere you are told to drive to), and `MAX_PARKING` caps
parking's share of the rail whatever the query returns. Same route after: 16
items. The cap is the load-bearing half — the query narrowing is a guess about
tagging conventions that will be wrong in some city, and the cap holds regardless.

## Rejected: road class as a stand-in for the speed limit

`routing::RoadKind::speed_kmh` exists and is free, and it is not a speed limit —
it is the cost the planner assumes for a road class. Showing it in the sign would
have cost nothing and been wrong on every 30 zone, school street and motorway
roadworks, *confidently*, in the shape of a real road sign. A missing sign is a
driver looking at the road; a wrong sign is a driver believing it. The limit comes
from OSM `maxspeed` in the corridor query or it is absent.

For the same reason `route_ahead::parse_maxspeed` refuses the implicit zone codes
(`NL:urban`, `DE:rural`) that are a large minority of `maxspeed` values. Resolving
them means shipping a per-country default table that changes by legislation and
silently rots; the tag says "whatever the law says here", and we do not know that.

## Rejected: feeding the rail from the streamed data layers

`data_layers` already streams POIs, so reusing them looks free. It is not: driver
mode's minimal mode turns every layer off (that is the point of it), and layers
stream in a grid around the *camera*, so the rail would only ever know about the
few hundred metres you are already looking at. The far two thirds of the route —
the part the widget exists to show — would be blank. Hence a corridor query along
the route polyline instead, once per route, cached by the query string.

## Not implemented: lane guidance

Both references show lane arrows before a junction. There is no data for it. OSM
`turn:lanes` is not in the Shortbread vector tiles the router is built from, so it
would need a third corridor query and a per-junction lane model, and the useful
version also needs to know which lane *you* are in. Left out deliberately rather
than approximated.

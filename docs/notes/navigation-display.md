---
topic: navigation
triggers: [driver.rs, route_ahead.rs, navigation.rs, "route rail", "speed limit", maxspeed, "driver mode", "POIs along the route", "off route", reroute, "route to", "context menu", arrived, "chase camera", "re-centre", peek, countdown]
updated: 2026-08-08
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

## Leaving the route is a request, not an error

"Off route — 140 m from the route" was the display giving up at the one moment it
is actually needed: the driver is somewhere unplanned and the manoeuvre card has
nothing true to say. Missing a turn is the normal case, so it now re-solves from
the current position. Three things this depends on:

- **Only the start moves.** The destination is what the user asked for; a reroute
  that quietly changed it would be unforgivable, and re-anchoring the start is the
  entire request as far as the solver is concerned.
- **The cooldown is the loop breaker, not politeness.** A solve can legitimately
  return a route the vehicle is still `OFF_ROUTE_M` from — the start snaps to the
  nearest routable road, and beside a dual carriageway or a closed street that can
  be the *other* road. With no floor between attempts the display re-solves every
  frame it stays there and never settles on a line.
- **"Rerouting" is tracked by route generation, not by `SolveStatus`.** Status is
  also true for every unrelated re-solve — switching travel mode mid-drive re-runs
  the search — and the card would then announce "Rerouting… from your current
  position" for something the user had just asked for deliberately.

## Arriving is a latched state, and it switches the reroute machinery off

The first cut had no end. The remaining distance ran down to zero, the card sat on
"Arrive · 0 m" for as long as you left it, and the only exit was a button on a
strip a docked panel could hide. Three things about the fix are load-bearing and
would otherwise be re-litigated:

- **Latched, not recomputed.** Driving the last few metres *past* the pin would
  take the arrival card away again, and the last thing a navigation display should
  do at the end of a trip is change its mind about having finished. Cleared only
  when the route generation changes — a reroute or a new destination is a new trip.
- **It disables the reroute.** Parking at the destination puts you tens of metres
  off the line by design (the route ends at the nearest routable road, the pin is
  where the user pointed), which is exactly the condition that asks for a fresh
  route. Left armed, the display answers arriving by planning a route to where you
  already are.
- **`ARRIVE_M` is generous (30 m) on purpose.** Pin-to-road is a building's depth
  often enough that a tight radius leaves a driver parked outside the address being
  told to keep going.

Ending the trip does **not** clear the route line. Clearing is the planner's act
(`driver.rs`'s module note says the same thing about driver mode versus planning),
and a driver who has just parked may well want to look at what they drove.

## The locked camera is not frozen, and the gestures cannot be handed back

A driver looks ahead — at the junction after this one, at where the car park
actually is. A rig that cannot be moved at all makes that impossible, so the
wheel, the drag and the pinch accumulate as offsets on the rig (`driver::Peek`)
and expire after `PEEK_TIMEOUT`.

**Handing them to `free_camera` by clearing `drive_locked` is the obvious
implementation and it does not work.** That system's Avatar arm clamps the
third-person zoom to `AVATAR_CAM_MAX` — 25 m, a tenth of a navigation chase
distance — so the first frame of a peek slams the camera onto the roof of the car
and every later frame drags it back there. Driver mode owns the rig, so it owns
the rig's input.

Two consequences worth keeping: the offsets sit *on top of* the rig rather than
replacing it, so the orientation cycle, the speed stretch and the junction tighten
all keep working underneath a peek; and the expiry is what makes reaching for the
camera safe while driving, because the driver never has to remember to undo it.
The Re-centre button is an offer, not the mechanism.

## The chase rig rides the speedometer, and closes on manoeuvres only

One pose cannot serve both ends of the speed range: tuned for a 30 km/h street it
shows about four seconds of road at 120, and tuned for the motorway it is a map of
the neighbourhood while you are hunting a house number. So the distance stretches
with speed up to a reference (`RIG_SPEED_STRETCH` / `RIG_SPEED_REF_KMH`) and
tightens onto the junction over the last `RIG_JUNCTION_M`.

The tighten is gated on the manoeuvre being one you *do* (`Progress::turn_ahead` —
not a `Continue` or a `Depart`). Without that gate the noded graph's mid-leg
vertices pull the camera in for every bend and the view breathes its way down the
road. Both effects fold into the same eased target the orientation writes, so
neither can produce a cut.

## Distance to the turn is a countdown, not a measurement

`format_distance` rounds to 10 m at every scale, which is right for a fact being
reported and wrong for a number being watched: on a kilometre of motorway the one
element of the card that should change only when something *has* changed repaints
every few metres. `driver::countdown` coarsens with distance — tens in the last
hundred metres, fifties through the rest of a kilometre — and stops naming a
distance at all inside `NOW_M`, where "10 m" describes a turn already being taken.
It rounds before choosing the unit, because rounding 980 m to the nearest 50 gives
"1000 m".

## "Route to" is the act; "route from" is the exception

The planner presented two identical endpoint fields, so every route was a
two-field form in which one field had an obvious answer the app already knew. The
start now defaults to the current position and renders as a one-line footnote
above the destination, which is the only field the user fills. `default_start`
fills it **only when empty** — refreshing it as the camera drifts would invalidate
and re-solve the route every frame, and stamping over a start the user picked
("Route from here") would silently undo them.

The same asymmetry is in the map's right-click menu: "Route to here" is a plain
button, "Route from here" a small one. Both go through `RouteState::set_endpoint_at`
so they cannot drift from the panel's own "pick on map" — an endpoint set without
the invalidate leaves the previous line drawn under a new destination, and without
the reverse geocode it stays labelled with raw coordinates.

That menu required a `Region` entity to exist before it would open at all
(`picking.rs`), and a region only exists when a sim has sent one — so `--offline`,
which is every `--shot` run, had no map right-click whatsoever. It now always
opens and the prim entries gate on `hit_region_id` instead.

## The trip strip cannot hide behind a panel

It is anchored to the right edge, which is where the docked panels are, and egui
paints floating `Area`s *above* panels — there is no z-order between an `Area` and
a panel to lose (the same constraint the place-pin culling in `place_pins.rs`
works around). So it stands down while a right panel is open rather than drawing
over the route's own turn list. The manoeuvre card, compass and speed pill are
clear of that edge and stay put.

That takes the **Stop** button with it, which is why Escape leaves driver mode
too: opening the Places list mid-drive otherwise removed the only way out. It is
gated on `EguiManager::ui_owns_keyboard`, so typing an address does not end a trip.

## Not implemented: lane guidance

Both references show lane arrows before a junction. There is no data for it. OSM
`turn:lanes` is not in the Shortbread vector tiles the router is built from, so it
would need a third corridor query and a per-junction lane model, and the useful
version also needs to know which lane *you* are in. Left out deliberately rather
than approximated.

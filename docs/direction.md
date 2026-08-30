# Direction

What this project is for, and which part of the work may proceed while nobody is
watching. Sessions read this before choosing what to do. A session may **propose**
changes to it as a ticket; it may not rewrite it.

## The goal

**A three-dimensional, augmented-reality twin of the real world — the Netherlands
in the most detail, because its government data is free and unusually complete.**

Everything else follows from that sentence. "Twin" is the load-bearing word: the
content comes from registries, never from invention. Where a registry covers a
place, the world is what the registry says; where none does, the world degrades
to OpenStreetMap and stays standing.

## Values

Seven sentences, and they are here to be *used*: when a ticket is ambiguous,
these decide it. A session working unattended resolves the question from this
list and writes down which value it applied. It does not stop to ask.

1. **Measured beats plausible.** Prefer a surveyed fact to a convincing
   invention, every time — a scattered tree where a real one is registered is
   the thing this project exists to stop doing.
2. **Official sources first.** PDOK, the Kadaster, the BAG, the BGT, AHN, 3DBAG
   — public registers with a licence and an interface. OpenStreetMap is the
   global fallback, not the preference, and a scrape is a last resort.
3. **Degrade, never break.** Coverage is national and the world is not. Outside
   it the ground goes flat and the buildings are extruded footprints; nothing
   disappears and nothing errors.
4. **Add, don't replace.** New data joins what is drawn. Substituting a source
   for a working layer deletes content wherever the new one has no coverage —
   which is usually exactly where somebody is standing.
5. **The frame budget is not negotiable.** Every streaming path is bounded per
   frame, in bytes as well as items. An unbounded upload burst is how this
   codebase loses a graphics device, and it is not a trade to make for detail.
6. **Ship the smallest thing a machine can judge.** Counts, placement, settle
   time, draw calls, memory. If the only way to tell whether it worked is to
   look at it, it is the wrong size or the wrong ticket.
7. **Say what you decided.** A choice made from this list goes in the commit
   body; a fact that will still be true in ten commits goes in `docs/notes/`.

## Why this makes an unattended lane possible

Most of the last year of work has one shape: *take a public dataset and raise it
into the world.* Surveyed trees and street lighting, the building register, the
pavements, measured ground relief, live transit, listed buildings.

That shape is automatable, and almost nothing else here is, because it can be
judged without taste:

- the number of things placed in a cell, and where they stand,
- whether the scene still settles, and how long it takes,
- draw calls, the largest single upload, resident graphics memory,
- whether an existing layer's counts moved.

All of that is already measured — every field is in `--dump-state`, and
`--expect` turns any of it into a gate that fails the run. On this hardware those
numbers are trustworthy. Colour, lighting and exposure are not
(`notes/headless-shots-software-renderer.md`), which is exactly why they are not
in the list.

## The frontier

The queue an unattended session pulls from. Wired means it reaches the client
today.

**Wired:** OpenStreetMap (vector tiles + Overpass) · 3DBAG LoD2.2 buildings ·
BAG building register, addresses and entrances · BGT street objects and paving ·
AHN ground relief · OVapi live transit · RCE listed buildings and protected
districts · a single IFC building model · forecast weather ·
PDOK aerial imagery (off by default — [notes](notes/aerial-imagery.md)) ·
RVO crop parcels, so a field is drawn as the crop grown on it rather than as one
generic green — client half only until cartopy's `brp.ts` serves a cell
([notes](notes/crop-parcels.md)).

**Candidates, unverified.** Nobody has yet confirmed the interface, the licence
or the volume for these — that check is the first half of the ticket, and
"this one is not worth it" is a valid result to record here:

| Source | What it would add |
|---|---|
| Rijkswaterstaat vaarwegmarkeringen, PDOK OGC API | Every buoy and fixed beacon on the national waterways, each carrying its colour pattern and the colour and character of the light it shows, so a river or a canal holds the objects that are really standing in it: 5 floating and 3 fixed marks in 13 KB over a Hollands Diep cell. |
| ProRail Spoorwegen, PDOK OGC API | Track axes, switches, level crossings and kilometre posts for the whole rail network, none of which the OSM rail ribbons this client already draws carry: 22 track axes and 70 switches, 208 KB, over the Groningen station cell, against one axis and no switches a cell to the north. |
| Rijkswaterstaat Weggegevens, PDOK OGC API | Surveyed lane counts and posted speed limits per section of the state road network, which is the measured answer to a width this client derives from a road's class: 8 sections and 8.9 KB over an A28 cell, each carrying a lane description such as 2 -> 2 and a limit with the hours it applies. |
| Kadaster BRT geografische namen, PDOK OGC API | The official name register — settlements, districts, water bodies and named buildings, each with geometry and a type — which is what could be named above street level, where the street labels stop and nothing is written on the map at all: 11 names in 4.7 KB over a Groningen cell. |
| RDW national parking register, opendata.rdw.nl SODA API | Parking garages as places with a known size rather than buildings like any other: position, capacity, height limit and charging points, 237 garages nationally in one 72 KB request, with a second table giving capacity per on-street parking area. |
| OpenSky Network live aircraft, REST API | Live aircraft positions, altitudes and headings, so the sky over the twin carries the traffic the real one does: one keyless request per view, 24 aircraft in 3.2 KB over a bounding box covering the whole country. |

**Checked and not taken.** The interface, licence and volume were confirmed and
the answer was no. Kept here so the check is not repeated:

| Source | Checked | Why not |
|---|---|---|
| Kadaster parcels | 2026-08-29 | A cadastral boundary is a legal line, not a physical object: **312.4 km** of it over a Groningen cell against **21.9 km** of surveyed fence, wall and hedge the BGT already supplies and `LineKind` already draws — so 93% of it would be an invented object where the register says nothing. The row's description was wrong too: a parcel is the whole plot, and the gardens are the BGT's `begroeidterreindeel`. CC0, and `Perceel` alone is 14.0 MB per cell. [notes/kadaster-parcels.md](notes/kadaster-parcels.md) |
| NDW traffic | 2026-08-29 | Real vehicle counts, from 20,519 loop detectors on motorway mainlines. Zero of them fall inside the 260 m radius the ambient cars are placed in at the Grote Markt, the Dam or Domplein, so substituting measurement for the generated traffic empties every city street — value 4. There is no bbox query either, so a per-cell service means re-slicing a national 52 MB file every minute, and a loop carries no `wvk_id` and no OSM id to join on. CC0, so the licence was never the obstacle. [notes/ndw-traffic.md](notes/ndw-traffic.md) |
| NWB road network | 2026-08-29 | Centrelines with administrative attributes. No width, no carriageway polygons, no bridge or tunnel geometry — the row's description was wrong. Its one unique field, `rel_hoogte`, answers a question `grade-separation.md` already closed without it. CC0, so the licence was never the obstacle. [notes/nwb-road-network.md](notes/nwb-road-network.md) |
| CBS neighbourhood statistics | 2026-08-29 | A table, not a picture. The only geometry is the buurt outline — an administrative line with nothing standing on it — and **89% of a cell is the statistics table** (257 KB of `buurten` against 27.6 KB of geometry; 244 attributes per feature, 30% of the values suppressed as `-99997`). Every consumer already reads something finer: window occupancy is per building from the BAG, the car and walker pools are sized for "is this street alive" and already denser than the measured car figure, and CBS counts residents and registered vehicles rather than who is on the street. CC0, and small. [notes/cbs-neighbourhood-statistics.md](notes/cbs-neighbourhood-statistics.md) |
| BGT, the remaining layers | 2026-08-29 | Mostly not taken, five exceptions. Of the **49** collections, seven were already read and **five more are worth reading** — `scheiding_vlak` and `vegetatieobject_lijn` as boundaries, `bord`, `kast` and `bak` as street objects, all forms `crates/geo/src/furniture.rs` already has; the client half of those has landed, the extractor half is `server/pipeline/bgt.ts` in cartopy. The other thirty-seven are not worth it, each reason in [notes/surveyed-street-objects.md](notes/surveyed-street-objects.md); `overbruggingsdeel` and `tunneldeel` (bridge decks, tunnel bodies) are split into a ticket of their own, needing a deck height the register does not carry. Interface unchanged (keyless, cursor-paged, historical versions returned), CC0 1.0 stated by the API itself. |
| Rijkswaterstaat water levels | 2026-08-30 | Live levels in cm NAP, keyless, from **705** water-height stations — and the client has nowhere to put them. A water surface takes one height for the whole cell, filled in from the banks because AHN does not measure water, and it is a rung of an **8 mm** coplanar stack laid over a clipmap that is eight opaque layers deep with nothing cutting holes in it. The measured drop over the Groningen Diepenring is **two to five metres**, so the canal would go under the basemap, and there is no bank or quay geometry in this tree to close the gap — value 5 and value 3. The join fails too: a station carries no water-body identifier at all, so every polygon's level would be a nearest-point guess. CC0, and the licence was never the obstacle. [notes/rijkswaterstaat-water-levels.md](notes/rijkswaterstaat-water-levels.md) |
| 3D basisvoorziening | 2026-08-30 | **There is no LoD3 in it** — the row's description was wrong. The buildings collection ships LoD 0 / 1.2 / 1.3 / **2.2** and semantics of `WallSurface` / `RoofSurface` / `GroundSurface` only, with no openings; its `rf_*` attributes and `NL.IMBAG.Pand.…` ids say it *is* 3DBAG, the dataset `lod22.rs` already streams — so taking it substitutes one delivery of one dataset for another, value 4, with value 6 agreeing that no `--dump-state` count would move. No national LoD3 exists anywhere, so `lod3.rs` has nothing to be replaced by. The other seven collections are AHN, the BGT and the point clouds, each already covered. No per-bbox feature query either: `/items` is a sheet index of 2 km zips. CC BY 4.0, the same licence the client already carries for 3DBAG, so the licence was never the obstacle. [notes/3d-basisvoorziening.md](notes/3d-basisvoorziening.md) |

## What an unattended session may do

Take the top unblocked frontier item, or a finding from the scheduled sweep.
Write it as a ticket first; build only what the ticket says.

Since 2026-08-29 that lane runs to the end: a ticket the watcher builds is
merged into `main` and deployed without anyone approving it. The point is that
work continues while the maintainer is unavailable, so every gate has to be a
machine one. Three are load-bearing:

- **Only a real green verdict ships.** `agent:done` is also reached when CI
  never answered within the timeout — that path lands a branch for review and
  must never reach the deploy, or a silent runner failure publishes untested
  code. The ship step re-reads the commit status and refuses anything that is
  not `success`.
- **The simulator deploys before the clients.** They share one wire protocol and
  a client one revision ahead of the server is refused at the handshake, which
  reads to a user as "connection reset".
- **Android is not on this path.** The published app is the least-tested build,
  an install is a thing a person did, and a bad one cannot be reverted for
  anybody who already has it. It stays a deliberate act.

## What it may not do

- **Change the wire protocol.** `PROTOCOL_HISTORY` is out of bounds. Every
  installed copy of the app talks that protocol, and no deploy from here reaches
  an app already on a phone.
- **Change the database schema.** A migration runs once against the live world
  and a revert does not undo it.
- **Anything you have to look at.** Colour, lighting, weather feel, the shape of
  a panel, whether a place reads as that place.
- **Augmented reality itself.** It is not in this codebase at all today, and
  nothing here has a camera or a headset. That thread needs a device and a
  person holding it.
- Publishing the Android app, the host machine, and the release signing key.


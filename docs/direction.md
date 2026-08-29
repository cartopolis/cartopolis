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
PDOK aerial imagery (off by default — [notes](notes/aerial-imagery.md)).

**Candidates, unverified.** Nobody has yet confirmed the interface, the licence
or the volume for these — that check is the first half of the ticket, and
"this one is not worth it" is a valid result to record here:

| Source | What it would add |
|---|---|
| BGT, the remaining layers | the surveyed surfaces not yet read |
| NDW traffic | real vehicle counts in place of invented ambient traffic |
| CBS neighbourhood statistics | how many people are actually there |
| Rijkswaterstaat water levels | canal and river surfaces at their real height |
| 3D basisvoorziening | national LoD3, in place of one hand-imported model |

**Checked and not taken.** The interface, licence and volume were confirmed and
the answer was no. Kept here so the check is not repeated:

| Source | Checked | Why not |
|---|---|---|
| Kadaster parcels | 2026-08-29 | A cadastral boundary is a legal line, not a physical object: **312.4 km** of it over a Groningen cell against **21.9 km** of surveyed fence, wall and hedge the BGT already supplies and `LineKind` already draws — so 93% of it would be an invented object where the register says nothing. The row's description was wrong too: a parcel is the whole plot, and the gardens are the BGT's `begroeidterreindeel`. CC0, and `Perceel` alone is 14.0 MB per cell. [notes/kadaster-parcels.md](notes/kadaster-parcels.md) |
| NWB road network | 2026-08-29 | Centrelines with administrative attributes. No width, no carriageway polygons, no bridge or tunnel geometry — the row's description was wrong. Its one unique field, `rel_hoogte`, answers a question `grade-separation.md` already closed without it. CC0, so the licence was never the obstacle. [notes/nwb-road-network.md](notes/nwb-road-network.md) |

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


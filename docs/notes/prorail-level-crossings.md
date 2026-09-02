---
topic: prorail-level-crossings
triggers: [crossings.rs, overweg, ProRail, Spoorwegen, spooras, wissel, kilometrering, "level crossing", overwegbomen, AHOB, AOB, azimut, levenscyclus_status, aantal_sporen, body_mentions_rail, WayFilter]
updated: 2026-08-30
---

# ProRail level crossings (PDOK Spoorwegen, `overweg`)

Every level crossing on the Dutch rail network, with the kind of installation
standing on it and how many tracks the road crosses.
`cartopolis_geo::crossings` is the pure half, `systems::crossings` the fetching
half.

## The interface, and what it costs

`https://api.pdok.nl/prorail/spoorwegen/ogc/v1` — **keyless** OGC API Features,
`bbox` query, GeoJSON out (storage CRS EPSG:28992, **CRS84 is what comes back by
default**, longitude first). `Access-Control-Allow-Origin: *` and
`Cache-Control: public, max-age=3600`, so the web build reaches it directly with
no relay. `limit` is clamped at 1000 (asking for 5000 answers `200` and returns
1000), cursor-paged through a `rel="next"` link, and `numberMatched` is never
returned. `properties=` subsetting is a **400** — the same shape
`systems::nav_marks` found on the RWS service, so a feature arrives with all
fifty of its administrative properties or not at all.

**Licence: CC0 1.0**, from the service's own landing page `rel="license"` link.

Per z14 cell, measured 2026-08-30 (`overweg` only):

| cell | features | bytes |
|---|---|---|
| Groningen, Peizerweg `8490/5321` | 1 | 2,444 |
| Utrecht Centraal `8424/5405` | 8 | 13,374 |
| Groningen Grote Markt `8490/5320` | 0 | 871 |
| Veluwe `8453/5400` | 0 | 877 |

Nationally: **3,917 features, 4 pages**. The empty answer is what the rail gate
below exists to avoid paying for over the rest of the planet.

## Six of the seven collections were rejected

The service publishes seven. Only `overweg` reaches the client. The rest were
checked on 2026-08-30 and the reasons are recorded here so the check is not
repeated — `docs/direction.md`'s values in brackets:

- **`spooras` (track axes) and `trace` (corridor centreline)** — drawing them
  lays a second set of rail ribbons over the ones `road_texture` already draws
  from OSM, which is the coplanar z-fight this codebase keeps paying for.
  *Substituting* them for the OSM rail is the argument `paving.rs` already lost
  for road surfaces: the ProRail axis carries no texture, no casing and no rung
  on the depth ladder, and it would delete every railway outside the
  Netherlands. **Value 4, add don't replace.** (22 / 68 features per station
  cell.)
- **`wissel` (switches) and `kruising` (diamond crossings)** — real geometry and
  real attributes (`hoekverhouding` 1:9, `max_snelheid_afbuigend`), and nothing
  to draw them *on*: the rails are a painted texture, not modelled steel, so a
  switch is invisible at every altitude and no `--dump-state` field would move
  for anything a person could see. **Value 6, ship the smallest thing a machine
  can judge.** Worth revisiting only if rails are ever modelled as geometry,
  which is a different ticket.
- **`station`** — already a POI through `places.rs`.
- **`kilometrering` (km posts)** — a small unlabelled plate on a post, with
  `rotatie` null on all 6,000 features sampled, so it has no orientation either.
  `furniture.rs`'s own rule applies: a kind that would need a texture, a curve or
  a survey to read correctly is better left undrawn. An unlabelled km post is a
  stick.

## Three things the register says that it does not mean

Each of these is invisible when it is got wrong, which is why they are here and
not only in a comment.

**`levenscyclus_status` is not a built/not-built flag.** 2,612 features read
`Bestaand`, 1,304 read `Definitief ontwerp` and one reads `Onbekend`. The
Peizerweg crossing in Groningen reads `Definitief ontwerp` and has been standing
in the street for years, as do every sampled `spooras` and `wissel` feature.
Filtering on it — which is the obvious first thing to do with a field of that
name — would delete a third of the register. This is the trap
`surveyed-street-objects.md` records for the BGT's retired trees, in the
**opposite** direction: there an obvious filter had to be added, here an obvious
filter must not be. `systems::crossings` does not read the field at all, and the
captured Peizerweg fixture is what pins it.

**`aantal_ahob` is not a boom count.** It is 100% filled and the single-track
Peizerweg crossing reports **15**. `aantal_aob` reads 0 on the sampled `AOB`
features. These are asset tallies of some kind; how many booms an installation
has comes from its form.

**`azimut` is null on all 3,917 features**, as are `type_hek` and
`aantal_andreas` — so the register records neither the orientation, nor the
fencing, nor the St Andrew's cross. The orientation has to be derived, which is
the one genuinely new piece of logic in the layer (below).

## The published extent is the projection's, not the register's

The `overweg` collection document's `extent.spatial.bbox` is
`-1.657292, 48.040502, 12.431727, 56.11059` — Rennes to Copenhagen, taking in
Belgium, northern France, Germany and Denmark. That is the RD grid (EPSG:28992,
the collection's storage CRS) reprojected to CRS84, not a statement about where
ProRail has railways. Using it verbatim as the coverage gate would let every cell
in five countries through.

The envelope actually used is **measured** over all 3,917 features:
`lon 3.593 … 7.350`, `lat 50.756 … 53.453`, padded to `3.5 … 7.4` by
`50.7 … 53.5`. `systems::aerial` already records the general rule — coverage is
an envelope, never a status code — and this is its sharper form: the envelope a
service publishes may describe its *grid* rather than its data.

It is still an envelope and not a border. A rectangle drawn round the Dutch
corners contains Brussels and Cologne, and each of those costs one empty request
if a cell there has a railway in its tile. A gate tight enough to exclude them
would clip Zeeland, which is the wrong trade in the wrong direction.

There is a further gate that is about the *service* rather than the cell — a 3 s
bound on the request and a per-service breaker with a minute's cooldown, because
this fetch sits between the cell's tile and its tessellation worker. It is shared
with the navigation marks and is written down once, in
[optional-layers-on-the-critical-path.md](optional-layers-on-the-critical-path.md).

## The census, 2026-08-30

`overweg_type` (3,917, 100% filled):

`Geen` 2,035 · `AHOB` 1,157 · `AHOB-MINI` 381 · `AOB` 137 · `ALI` 63 ·
`Onbekend` 40 · `AKI` 24 · `HALI` 21 · `WILO` 16 · `VKL` 9 · `HAVIO` 9 ·
`HBKI` 8 · `HAHOB` 5 · `EBO` 4 · `ALI-B` 3 · `HALI-B` 3 · `HAKI` 1 · `ADOB` 1.

`CROSSING_TABLE` names eleven of the eighteen and draws nothing for the rest.
`Geen` is a crossing with **no installation** — a farm track over a branch line
with a sign and nothing else — and drawing anything there is the single biggest
way this layer could lie; `Onbekend` is the register saying it does not know.
The other five left out (`WILO`, `VKL`, `HAVIO`, `EBO`, `ADOB` — 39 features
between them) are codes whose expansion is not established; `VKL` in particular
looks like road traffic signals rather than a crossing installation. They are
recorded here so the next reader knows they were considered rather than missed.

`karakter` (100% filled): `Openbare overweg` 2,022 · `Onbekend` 587 ·
`Dienstoverpad` 557 · private 263 · `Niet-openbare overweg met een openbaar
karakter` 214 · `Openbare overweg (voetpad)` 158 · … Not read: the form comes
from `overweg_type`, and the road's own width is already in the tile.

`aantal_sporen` (100% filled): 1 → 2,091 · 2 → 1,269 · 3 → 232 · 4 → 126 ·
0 → 68 · 5 → 48 · up to 14. It sets how far apart the two installations stand
and nothing else. Zero is read as one — a crossing nobody counted, not a
crossing over no track — and the count is clamped at 16.

`vri` (road signals present): `Ja` on 24. Not read.

## Orientation comes from the road, and the rail has to be excluded to get it

The collection's own description says it plainly: *"Dit punt is dus niet het
midden van een overweg maar een punt op de spoorhartlijn in de buurt van een
overweg"* — the point is on the **rail centreline** near a crossing, not the
middle of one. Two things follow.

`furniture::WayField` answers "which way does the nearest way run" and is what
turns a bench toward its path, but it indexes *every street, path and railway* in
the tile and reads no `kind` at all. Asked about a crossing's own point it
answers the **track**, every single time, and every boom would be laid along the
rails it is supposed to bar — a failure that looks entirely plausible from above.
So the field grew a `WayFilter`, and `road_texture::is_rail_kind` (beside
`road_class`, so the two cannot drift) is what it excludes. The test that pins it
builds a field with one east–west track and one north–south road crossing it and
asserts the yaw at the rail point is the road's.

And the placement is approximate at the metre level by the register's own
admission, which is why the search radius is 30 m rather than the furniture's 20.
A crossing with **no non-rail way in range is dropped**, not drawn at a guessed
angle — a real loss (a service crossing over a farm track OSM does not carry goes
undrawn) and the honest one. `level_crossings` in `--dump-state` therefore counts
what is on screen rather than what came back from the register.

## What is not drawn, and why

- **The St Andrew's cross.** Every Dutch crossing has one; the register does not
  record it (`aantal_andreas` null on all 3,917). Drawing it would be plausible
  rather than measured — value 1.
- **Animation.** A boom that lowers needs a train, and there is no train. The
  booms stand raised, which is what a crossing looks like almost all the time.
- **Colliders.** Crossings are not `FurnitureInstance`s and reach nothing in
  `collision::sync_prop_colliders`, exactly as the navigation marks do not.
- **A Layers toggle of its own.** Crossings ride the existing Furniture toggle,
  because they are that group's mesh, material and altitude fade.

## Deferred measurements

Not taken, none of them blocking:

- Bytes and wall clock of a warm versus cold cell fetch on the streamed ring.
- The national `kilometrering` total; paging stopped at 6,000 in 6 pages.
- How many `streets` features with `kind = rail` a Shortbread z14 tile carries
  over Groningen station. Only needed if somebody re-proposes `spooras`.
- Whether the installation *reads* as a level crossing at walking distance.
  Geometry, placement and counts are trustworthy on this project's headless
  renderer; colour and lighting are not
  (`headless-shots-software-renderer.md`), so that judgement belongs on a
  workstation or the Android emulator.

---
topic: plans
triggers: [plans.rs, crates/atlas/src/plans.rs, systems::plans, "/plans/groningen.json", plans_refresh, CARTO_PLANS_URL, bekendmakingen, Gemeenteblad, gemeenteraad, raadsinformatie]
updated: 2026-10-30
---

# Plans: notices and council business on the map

Layers ▸ Places ▸ Plans draws what Gemeente Groningen publishes about a place,
at the place, and the council business that names it. Atlas builds one feed
from three open sources (`atlas::plans`), the client draws it
(`systems::plans`). The design is `docs/mockups/plans-and-decisions/`.

## The sources, measured 2026-10-30

- **Notices: KOOP's SRU** (`repository.overheid.nl/sru`, no key, CORS `*`).
  `dt.creator=="Groningen"` alone also returns the **Provinciaal blad** — 75 of
  662 records in October 2026 — so the query adds
  `w.publicatienaam=="Gemeenteblad"` and the parser checks it again. 62,356
  municipal records in all; 1,336 from 1 Aug to 29 Oct 2026, of which 96 %
  carry a point, line or area in WGS84 (`overheidwetgeving:locatiegebied`) and
  1,095 a `datumEindeReactietermijn`. A year is 5,520 notices; the feed is
  2.1 MB of JSON. Same-day: notices of the 29th were in on the 30th.
- **Coordinates come in two orders.** `locatiepunt` and a bare `locatiegebied`
  are `lat lng` / `lat,lng`; WKT (`POLYGON((…))`, `LINESTRING(…)`) is `lng lat`.
  `geometrie` is RD and is not read.
- **Council: the council site's JSON** (GemeenteOplossingen,
  `gemeenteraad.groningen.nl/api/v1`). `/meetings?date_from=&date_to=` takes
  **Unix seconds** and answers every meeting in the range with its items and
  documents in one body (106 meetings, 950 kB for 14 months); a date string
  times out, and there is no other list endpoint (`/meetings`, `/dmus/1`,
  `/calendar` are 404 or hang). The site's HTML is behind an Anubis challenge;
  the API is not. Documents download from
  `/api/v1/meetings/{meeting}/documents/{document}`.
- **Open Raadsinformatie** holds the same council record with full text, but had
  nothing after 2026-07-01 when checked, and its `neighborhood_polygons` field is
  empty for Groningen. Not used.
- **Names: PDOK's location server**, `fq=gemeentecode:0014` and
  `type:(weg OR buurt OR wijk)`: 2,633 names, at most 100 rows a request.
  `fq=gemeentenaam:Groningen` is a text match and returns Midden-Groningen's
  streets too.

## Council items have no place, so they are placed by name

An agenda item's title is matched against the gazetteer, whole words, longest
first. That is a guess, and the client says so ("matched by place name",
"Placed by the name … in its title"). What the first live build taught:

- **"Groningen" is a neighbourhood name and must not match.** Left in, 4,357 of
  5,520 notices were linked to by-laws ("Afvalstoffenverordening gemeente
  Groningen"). It heads `NOT_PLACES`.
- A name in several towns keeps the city's or is dropped ("Hoofdweg").
- Only titles are matched, not documents: a budget's PDF names every street.
- The agenda committee, presidium and clerk's meetings list items again as
  procedure and are not steps.

## Not built yet

Following a place and being told of a new notice near it (v1 screen 4's
notification) — it needs a notification path that runs with the app closed.
Other municipalities: the notices query is one constant away, the council site
is Groningen's own.

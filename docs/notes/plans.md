---
topic: plans
triggers: [plans.rs, plans_watch, FollowPlans, PlansFollow, plans_follow, crates/atlas/src/plans.rs, systems::plans, "/plans/groningen.json", plans_refresh, CARTO_PLANS_URL, bekendmakingen, Gemeenteblad, gemeenteraad, raadsinformatie]
updated: 2026-11-17
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

## Following a place (protocol 50, built 2026-11-17)

*Plans here* has **Follow this place**. A followed place is a spot, not a
street: a notice whose pin is within 100 m of it (`plans::NEAR_M`, the same
"here" the card counts) is news.

- **In the app** news is a chip at the top of the map, "New near a place you
  follow", with the newest notice's title; a tap opens that notice and counts
  everything near its place as shown. What was published before the place was
  followed is not news. The app reads the feed while anything is followed, not
  only with the layer on. What counts as shown is the device's own
  (`UserData::plans_follow`).
- **With the app closed** the server wakes the phone: it reads the feed every
  15 minutes (`simulator::plans_watch`), keeps the ids it has checked
  (`plans_seen`, so a restart wakes nobody twice; its very first read wakes
  nobody for a year of notices), and for a notice it has not checked, dated in
  the last 14 days, wakes every account following a spot near it. The push is
  the word "plan" (`message-wake-ups.md` says why so little), only to phones
  registered as `plans:<address>` — an app older than this would show it as
  "Incoming call". Android shows "New plan near a place you follow"; opening it
  opens the app, which finds the notice itself.
- **The server is told the places** (`FollowPlans`, the whole list): once a
  connection when there are any, and on every change. A device that follows
  nothing says nothing, so it does not clear another device's places; two
  devices with *different* lists overwrite each other, the last to connect
  winning. 20 places at most. Deleting the account deletes them.
- **Where the server reads the feed:** `plans_url` (`CARTO_PLANS_URL`), else
  `atlas_url`'s `/plans/groningen.json`, else nowhere — and then nobody is
  woken, which it says at startup. The hosted server runs atlas as a separate
  unit and needs `CARTO_PLANS_URL=http://172.17.0.1:4749/plans/groningen.json`
  in `/etc/cartopolis-sim.env`.

**Not checked from here:** a real phone woken by a real distributor. The
server's half is tested with a database; the Android notice and the app's chip
were not seen on a device.

Not done: the browser is never woken for a plan (the web page follows no
places, and `sw.js` shows any word but "message" as a call); the notice's own
card has no Follow button (v1 screen 2), only *Plans here*; and a followed
place has no name — the chip names the notice's place instead.

## Not built yet

Other municipalities: the notices query is one constant away, the council site
is Groningen's own. Following only reaches Groningen for that reason.

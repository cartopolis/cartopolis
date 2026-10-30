---
topic: plans-and-decisions
triggers: [bekendmakingen, gemeenteblad, raadsinformatie, council decisions, permits, Plans layer, data_layers.rs]
updated: 2026-10-30
---

# Plans and decisions on the map

## v2 — Plans as a layer of its own, proposed 2026-10-30

![v2](v2-layer.png)

Asked for the same day: Plans as a separate layer to toggle. It is one tile
under *Places* in the Layers sheet as built (beside Public transport stops,
Shops, Friends and Community posts), with an *NL* badge. Off means nothing is
drawn or fetched. v1's filter chips on the map move behind the tile's *⋯*: the
kinds (open, decided, council, events, traffic) and how far back (open only,
3 months, a year). On the map only the count chip stays, and its ✕ is the same
switch as the tile. The ring's *Plans here* (v1 screen 4) works with the layer
off.

Asked to be built the same day ("ok maak het") and built as v2 draws it, with
these differences: the ⋯ filters are by kind of notice (applications,
decisions, notifications, events, traffic, other) plus a Council switch, and
the dot's colour alone says open or decided, rather than "open" and "decided"
being filters of their own; the council square carries a tick when the council
has decided. Following a place and its notification (v1 screen 4) are not
built (`docs/notes/plans.md` says why). v1's counts were corrected the same
day: the first query also counted the Provinciaal blad.

## v1 — proposed 2026-10-30

![v1](v1.png)

A *Plans* layer for Groningen: every notice the municipality publishes in the
Gemeenteblad (permit applications and decisions, demolition notices, events,
traffic orders) as a dot at the point, line or area the notice itself carries,
orange while its response period runs. Tapping one shows the notice, its
deadline and the council decision behind it; a decision has its own page from
proposal to the notices it led to; the map's ring gets *Plans here* and a place
can be followed for new notices within 100 m.

Everything drawn is real data, fetched 2026-10-30: the 22 notices published
2026-08-10 – 2026-10-29 within the drawn part of the centre, PDOK's current
aerial (`vismarkt.jpg`, Actueel_orthoHR, z18), and council item 5d of
2026-02-18, the Vismarkt maintenance credit, whose text names "the Vismarkt and
part of the Akerkhof" — the area of the 2026-10-28 paving application.

The sources and how fresh each was are in the table at the foot of the sheet.
The notices (KOOP SRU, `repository.overheid.nl/sru`) need no geocoding; the
council record (`gemeenteraad.groningen.nl/api/v1/meetings/{id}`) has no
coordinates and is linked by the place names in its text, which is why the
link is labelled *matched by name*. Open Raadsinformatie holds the same council
record but had nothing after 2026-07-01.

Not decided yet: whether to build it, how the name matcher is reviewed, and
whether events and traffic orders (routes across the whole centre) belong in
the same layer.

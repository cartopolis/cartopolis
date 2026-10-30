---
topic: plans-and-decisions
triggers: [bekendmakingen, gemeenteblad, raadsinformatie, council decisions, permits, Plans layer, data_layers.rs]
updated: 2026-10-30
---

# Plans and decisions on the map

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

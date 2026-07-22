# ADR-024: Live Transit Departure Boards (OVapi)

---
**Metadata:**
- **ID**: ADR-024
- **Status**: Accepted
- **Date**: 2026-07-11
- **Tags**: [data-layers, transit, live-data, ui, dependency]
- **Decision Type**: technology_choice
- **Aligns with**: [G-04, P-03]
- **Roadmap tier**: P1
- **Doc resolution**: R1
- **Related**: [ADR-023]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The transit data layer (ADR-023) shows *where* stops are, but a
stop is only alive if you can see *what's coming*: the next buses/trams with
real departure times. That needs a live feed, keyed per stop, and a correct
notion of current wall-clock time.

**Requirements**:
- Open, key-free source joinable to the OSM stops we already render.
- One cheap request must cover every stop near the camera (performance and
  politeness — this runs continuously while the layer is on).
- Departure countdowns must be computed against real time, DST-correct,
  without dragging in a timezone database.

## Decision

Poll **OVapi** (`http://v0.ovapi.nl`), the open real-time feed for all Dutch
public transport, and render an egui departure board anchored above each
nearby stop pin.

**Approach** (`systems/transit_live.rs`): OSM stops carry `ref:IFOPT`
(`NL:Q:10006090`); the numeric suffix is OVapi's timing-point code — a direct
join, no lookup table. A worker thread issues one batched `/tpc/{code,…}`
request per 30 s covering the ≤40 nearest stops within 800 m. A `WorldClock`
resource tracks unix time; OVapi's naive-local timestamps are converted using
the UTC offset the feed reports in `LastUpdateTimeStamp` (`+0200`/`+0100`), so
DST is the feed's problem and the client needs only `std` date math. Boards are
egui overlays projected from the pin anchors (max 8, nearest within 500 m),
showing line, destination, and minutes-to-departure with delay/cancellation
colouring.

## Rationale

**Primary Reasoning**:
1. **Zero-friction join**: `ref:IFOPT` is tagged on effectively every Dutch
   stop; verified live that codes resolve and OVapi coordinates match the OSM
   nodes within metres.
2. **Batching makes it performant**: one ~20 KB request per interval covers a
   whole screenful of stops (~120 ms measured), regardless of stop count.
3. **The feed is its own timezone authority**: parsing the reported offset
   avoids both a chrono/tz dependency and the classic naive-local-as-UTC bug.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| NDOV raw feeds (KV6/KV78turbo) | Authoritative, push-based | Registration, ZeroMQ plumbing, per-operator formats | Heavier than a client-side overlay needs |
| GTFS-Realtime aggregators | Worldwide pattern | No single open worldwide endpoint; per-feed keys and formats | Right direction for non-NL later, not for v0 |
| 3D text boards in-world | Prettier, diegetic | Needs a text-to-texture pipeline Bevy lacks | egui overlay ships now; 3D skin can replace the renderer later without touching the data path |

## Consequences

**Positive**:
- Stops read as *live*: next departures, delays (amber/red), cancellations.
- The `LayerDef::live_id` hook generalises: any future layer can bind a live
  feed the same way.

**Negative**:
- **HTTP-only**: `v0.ovapi.nl` has no TLS. Departures are treated as untrusted
  display strings only; they never influence world or sim state.
- **Netherlands-only**: stops without an `NL:Q:` IFOPT ref simply get no
  board. Worldwide coverage would mean GTFS-RT integration (new ADR).
- **Unofficial service**: OVapi is community-run with no SLA. Failures degrade
  to stale-then-absent boards (5 min TTL); nothing else depends on it.

## Related

- [ADR-023](./023-data-layers.md): The transit layer this attaches to; supplies the stop positions and `ref:IFOPT` codes.

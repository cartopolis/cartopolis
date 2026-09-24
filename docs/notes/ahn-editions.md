---
topic: ahn-editions
triggers: [AHN6, AHN5, AHN4, ahn6.rs, "/height", height.rs, terrain.rs, "basisdata.nl", "ellipsis-drive", "sheet index", "tunnel lid", "open trench", "survey date"]
updated: 2026-09-24
---

# Which flight of AHN the map stands on

## The survey date is a rendering bug

AHN is flown region by region over several years, and a map built on it shows the
ground **as it was on the day of the flight**. PDOK serves only AHN4
(`service.pdok.nl/rws/ahn/wcs/v1_0`), which over Groningen was flown while the
southern ring (N7) was an open trench. It has since been roofed over and parked on,
and every tunnel workaround in `docs/notes/tunnels.md` — filling the lids back in,
the 80th-percentile lid level, the covered-trench building filter — exists to undo
that one date.

Measured 2026-09-24, both editions sampled inside the BGT outlines:

| Where | AHN4 | AHN6 |
|---|---|---|
| Ring, main covered stretch (tunneldeel `L0002.3e70…`), DTM median | −7.41 m NAP | +2.42 m |
| Ring, motorway carriageway pieces, DTM | −7.3 to −7.7 | +2.4 to +2.6 |
| Emmaviaduct deck (rebuilt since), DSM median | 7.8 | 8.7 |
| Unchanged ground on the Diepenring, DTM, sample for sample | — | median 2.5 cm off, p90 5.7 cm |

The last row is the check that `ahn6.rs` reads a sheet the right way up and on
PDOK's pixel-edge convention (`ahn6_agrees_with_ahn4_where_nothing_changed`,
ignored by default because it downloads).

## Where AHN6 is

Not on PDOK and not a WCS. 1 km GeoTIFFs named for their south-west RD corner,
found through an OGC Features sheet index on Ellipsis Drive
(`api.ellipsis-drive.com/v3/ogc/features/0820faae…/collections/6aec07f5…/items?bbox=`),
whose properties link `basisdata.nl/hwh-ahn/AHN6/{02a_DTM_50cm,03a_DSM_50cm,…}` —
redirected to Hetzner object storage, no key. Per sheet: 0.5 m and 5 m, terrain
(`M`) and surface (`R`); float32, deflate, 256² internal tiles, no-data −9999.
~8 MB DTM and ~9.5 MB DSM a sheet; the LAZ is 348 MB.

**Coverage is partial and growing.** On 2026-09-24 Groningen, Zwolle and Delfzijl
had sheets; Amsterdam, Utrecht, Rotterdam, Eindhoven and Maastricht did not. Hence
the per-sample fallback to AHN4, and a sheet-index miss is cached for a week only.

**No licence was found stated for AHN6.** AHN4 on PDOK is CC0 and AHN has been open
data by policy; confirm before relying on it anywhere a licence matters.

The old AHN5 host (`ns_hwh.fundaments.nl`) answers NoSuchBucket, and the AHN5 sheet
index returned nothing over Groningen.

## What the AHN6 DTM still does

It still cuts bridge decks out (80–100 % no-data over the Emmaviaduct's decks), so
the deck height as DSM over the outline minus DTM around it carries over unchanged
(`bridge-heights.md`).

## Where AHN4 is still the better answer

Under a lid. AHN6 reads the roof; AHN4, where it was flown before the roof, reads
the road the tunnel still carries — the ring's carriageway at −7.3 to −7.7 m NAP is
the tunnel floor today. Nothing uses that yet.

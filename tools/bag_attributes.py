#!/usr/bin/env python3
"""Preprocess the **BAG** (Basisregistratie Adressen en Gebouwen) into per-cell
attribute files the client joins onto the buildings it already draws.

The 3DBAG pipelines next door supply a building's *shape*; this supplies what it
is. Every record is keyed on the BAG **identificatie** — the same 16-digit string
`3dbag_lod22.py` now writes as each mesh element's `id` — so the join needs no
geometry matching and no tolerance: a drawn building either carries the id or it
does not.

Source is PDOK's open BAG WFS, which needs no API key (`api.bag.kadaster.nl`, the
other route to the same register, does). Four layers:

  * `bag:pand` — one row per building: `bouwjaar`, `status`, `gebruiksdoel`
    (comma-joined for a mixed-use block), the floor-area range of the units
    inside and how many there are. This is the layer that carries almost
    everything, and it carries it *per pand*, so no join is needed to use it.
  * `bag:verblijfsobject` — one row per addressable unit: its own area and use,
    the full address, `pandidentificatie` (the join key), and a **point** that
    sits at or near the unit's entrance.
  * `bag:ligplaats` / `bag:standplaats` — berths for houseboats and plots for
    mobile homes. They are addressable BAG objects in their own right, with no
    pand and no 3D geometry, which is exactly why they are missing from a map
    built from buildings alone.

Pipeline (server-side, so the client stays in lat/lng like every other stream):

  1. Page each WFS layer over an RD (EPSG:28992) bbox (see `PAGE` — the server
     truncates harder than it admits).
  2. Reproject to WGS84 and bin into the client's zoom-14 cells. A pand goes into
     **every cell its footprint bbox touches**, not just the one owning a chosen
     vertex: the mesh pipeline bins by the first vertex of a 3D solid, a rule this
     script cannot reproduce from a 2D footprint, and a building whose attributes
     landed one cell over from its mesh would silently never join. Duplicating the
     handful of buildings that straddle a cell edge costs a few bytes and removes
     the failure mode entirely.
  3. Write `bag/{14}/{x}/{y}.json` + `bag/coverage.json` into the same served tree
     as the 3DBAG output.

Wire format — **two files per cell**, same schema, different arrays populated.
Street names interned per file, absent fields omitted:

    bag/{14}/{x}/{y}.json
    {"buildings": [{"id": "0014100010909097", "year": 1884, "use": 1,
                    "area": 94, "units": 1, "status": "demolished"?}, ...]}

    bag/{14}/{x}/{y}.places.json
    {"streets": ["Paterswoldseweg", ...],
     "addresses": [{"id": "0014100010942550", "street": 0, "nr": "62",
                    "postcode": "9726BG", "at": [53.2194, 6.5665],
                    "use": 1, "area": 156}, ...],
     "berths":    [{"kind": "mooring"|"plot", "street": 0, "nr": "1002",
                    "postcode": "9726AZ", "at": [lat, lng]}, ...]}

The split is not tidiness, it is who pays. Measured on Groningen's densest z14
cell: 8 000 buildings are 0.53 MB and the 17 785 addressed units in the same cell
are 2.04 MB. `lod22` fetches a cell's facts inside its own mesh job, because a
building's year and use are baked into vertex colours — and it has no use
whatever for the addresses. Keeping them in one file made the heaviest streaming
lane in the client pull five times what it reads. Addresses are fetched only by
`building_facts`, which is the half that answers "what am I standing next to".

`use` is the USE_BITS mask below rather than the register's Dutch strings: a pand
routinely has several uses, the set is closed and small, and the client wants to
test membership ("is anyone living here?") rather than to display the word. Keep
it in step with `systems::building_facts::BuildingUse` on the client — the two
halves of one enum in two languages is the seam this format is most likely to
drift at.

`status` is omitted for the overwhelmingly common "Pand in gebruik" and named
only when it is something else, so the field is both cheap and self-describing.

Data: Kadaster BAG via PDOK — open data, no licence restrictions, attribution
requested.

Usage:
  ./venv/bin/python bag_attributes.py --out site --name groningen \
      --bbox 229000 578000 238000 587000
"""

import argparse
import json
import math
import os
import re
import sys
import tempfile
import time
import urllib.parse
import urllib.request

from pyproj import Transformer

WFS_BASE = "https://service.pdok.nl/lv/bag/wfs/v2_0"
# Features per GetFeature request. **Measured, not the documented cap**: this
# service truncates a GeoJSON response at 1000 features however large a `count`
# you ask for, and — unlike the 3DBAG WFS that `3dbag_extract.py` pages, which
# answers 10 000 — it usually omits `numberMatched` from GeoJSON entirely. Asking
# for 10 000 therefore returns a *full* page that looks like a short one, and the
# usual "fewer than I asked for, so that was the last page" rule ends the walk
# after the first 1000 rows with no error anywhere. Hence this number, and hence
# `hits()` below to check the total independently.
PAGE = 1_000
# The other half of the same trap, and the one that actually stops a city:
# `startIndex` is refused outright past 50 000 (HTTP 400, measured 2026-08-27 —
# 50 000 answers, 50 001 does not). Groningen alone has 94 052 panden, so no
# amount of paging can read one bbox to the end. The answer is to split the bbox
# until each piece is small enough to page, which `split_bbox` does; this is the
# ceiling it splits to, with headroom because `hits` is counted before the walk
# and the register moves under us.
MAX_START_INDEX = 50_000
MAX_PER_BBOX = 40_000
# Depth guard for that recursion. Four levels is a 256-fold reduction, i.e. ~150
# features per piece at Groningen's density — anything still over the ceiling
# then is not a density problem and splitting further will not fix it.
MAX_SPLIT_DEPTH = 4
CACHE_ZOOM = 14          # must match osm_buildings::CACHE_ZOOM
_TO_WGS84 = Transformer.from_crs("EPSG:28992", "EPSG:4326", always_xy=True)

ATTRIBUTION = "Building attributes © Kadaster BAG via PDOK (open data)"

# The BAG's closed set of `gebruiksdoel` values as a bitmask. Order is the wire
# format; append, never renumber. Mirrored by `BuildingUse` on the client.
USE_BITS = {
    "woonfunctie": 1 << 0,
    "winkelfunctie": 1 << 1,
    "kantoorfunctie": 1 << 2,
    "bijeenkomstfunctie": 1 << 3,
    "industriefunctie": 1 << 4,
    "onderwijsfunctie": 1 << 5,
    "gezondheidszorgfunctie": 1 << 6,
    "logiesfunctie": 1 << 7,
    "sportfunctie": 1 << 8,
    "celfunctie": 1 << 9,
    "overige gebruiksfunctie": 1 << 10,
}

# `status` values worth telling the client about, shortened. "Pand in gebruik" is
# ~all of them and is the omitted default; the rest matter because a building can
# be drawn from 3DBAG while the register says it is gone or not yet there.
STATUS_CODES = {
    "Bouwvergunning verleend": "permit",
    "Bouw gestart": "building",
    "Pand in gebruik (niet ingemeten)": "in_use_unmeasured",
    "Verbouwing pand": "renovation",
    "Pand buiten gebruik": "out_of_use",
    "Sloopvergunning verleend": "demolition_permit",
    "Pand gesloopt": "demolished",
    "Niet gerealiseerd pand": "not_realised",
    "Pand ten onrechte opgevoerd": "withdrawn",
}
STATUS_IN_USE = "Pand in gebruik"


def lat_lng_to_tile(lat, lng, z=CACHE_ZOOM):
    n = 2.0 ** z
    x = int((lng + 180.0) / 360.0 * n)
    lat_rad = math.radians(lat)
    y = int((1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi)
            / 2.0 * n)
    return x, y


def use_mask(gebruiksdoel):
    """`"winkelfunctie,woonfunctie"` -> the OR of their bits (0 if unknown).

    An unrecognised value is skipped rather than mapped to "overige": the mask is
    read as evidence ("this is a home"), so inventing a use for a word we do not
    know would be worse than saying nothing about it.
    """
    mask = 0
    for part in (gebruiksdoel or "").split(","):
        mask |= USE_BITS.get(part.strip(), 0)
    return mask


def wfs_page(layer, bbox, start):
    """One page of GeoJSON features from a BAG WFS layer over an RD bbox."""
    xmin, ymin, xmax, ymax = bbox
    params = {
        "service": "WFS",
        "version": "2.0.0",
        "request": "GetFeature",
        "typeNames": layer,
        "count": str(PAGE),
        "startIndex": str(start),
        "outputFormat": "application/json",
        "srsName": "EPSG:28992",
        "bbox": f"{xmin},{ymin},{xmax},{ymax},EPSG:28992",
    }
    url = WFS_BASE + "?" + urllib.parse.urlencode(params)
    for attempt in range(4):
        try:
            with urllib.request.urlopen(url, timeout=180) as resp:
                return json.load(resp)
        except Exception as e:  # noqa: BLE001 - retry any transient failure
            if attempt == 3:
                raise
            print(f"  {layer} page {start}: {e} (retry {attempt + 1})", file=sys.stderr)
            time.sleep(3)


def hits(layer, bbox):
    """How many features the layer has over the bbox, or `None` if unreadable.

    `resultType=hits` answers in GML whatever `outputFormat` asks for, so this is
    a tiny XML read rather than a second full download. It exists to make the walk
    below *checkable*: without it a truncated page is indistinguishable from a
    finished layer, and the failure is a map that quietly knows about the first
    thousand buildings in a city.
    """
    xmin, ymin, xmax, ymax = bbox
    params = {
        "service": "WFS", "version": "2.0.0", "request": "GetFeature",
        "typeNames": layer, "resultType": "hits", "srsName": "EPSG:28992",
        "bbox": f"{xmin},{ymin},{xmax},{ymax},EPSG:28992",
    }
    url = WFS_BASE + "?" + urllib.parse.urlencode(params)
    try:
        with urllib.request.urlopen(url, timeout=120) as resp:
            head = resp.read(4096).decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001 - a missing total is not fatal
        print(f"  {layer}: hits unavailable ({e})", file=sys.stderr)
        return None
    m = re.search(r'numberMatched="(\d+)"', head)
    return int(m.group(1)) if m else None


def split_bbox(layer, bbox, depth=0):
    """Sub-bboxes each holding few enough features to page to the end.

    Quartered recursively until every piece is under `MAX_PER_BBOX` or the depth
    guard runs out. Note the pieces overlap in *content*, not in area: a WFS bbox
    is an intersects test, so a building straddling a cut is returned by both
    halves — which is why every caller dedupes on `identificatie`.
    """
    n = hits(layer, bbox)
    if n is None or n <= MAX_PER_BBOX or depth >= MAX_SPLIT_DEPTH:
        if n is not None and n > MAX_PER_BBOX:
            print(f"  WARNING: {layer} still has {n} features in "
                  f"{bbox} at split depth {depth}; the walk will stop at "
                  f"{MAX_START_INDEX} and this area will be incomplete.",
                  file=sys.stderr)
        return [(bbox, n)]
    xmin, ymin, xmax, ymax = bbox
    xmid, ymid = (xmin + xmax) / 2.0, (ymin + ymax) / 2.0
    out = []
    for quad in ((xmin, ymin, xmid, ymid), (xmid, ymin, xmax, ymid),
                 (xmin, ymid, xmid, ymax), (xmid, ymid, xmax, ymax)):
        out.extend(split_bbox(layer, quad, depth + 1))
    return out


def pages(layer, bbox):
    """Yield every feature of a layer over the bbox, in pieces small enough to page.

    Two independent server limits are in play and both fail quietly (see `PAGE`
    and `MAX_PER_BBOX`), so this walks each piece to a short page and checks the
    running total against `hits`. Features near a split line arrive twice; the
    collectors drop the repeat by identificatie.
    """
    pieces = split_bbox(layer, bbox)
    if len(pieces) > 1:
        print(f"  {layer}: split into {len(pieces)} bboxes to stay under the "
              f"{MAX_START_INDEX}-row paging limit", file=sys.stderr)
    grand = sum(n for _, n in pieces if n is not None)
    seen_total = 0
    for piece, total in pieces:
        start = 0
        while True:
            page = wfs_page(layer, piece, start)
            feats = page.get("features") or []
            if not feats:
                break
            yield from feats
            got = len(feats)
            start += got
            seen_total += got
            print(f"  {layer}: {seen_total}/{grand or '?'}", file=sys.stderr)
            if total is not None and start >= total:
                break
            if got < PAGE or start >= MAX_START_INDEX:
                break
        if total is not None and start < total:
            print(f"  WARNING: {layer} yielded {start} of {total} features for "
                  f"{piece} — that piece is incomplete.", file=sys.stderr)


def geom_coords(geom):
    """Every (x, y) in a GeoJSON geometry, at any nesting depth."""
    if not geom:
        return
    def walk(node):
        if (isinstance(node, (list, tuple)) and len(node) >= 2
                and all(isinstance(v, (int, float)) for v in node[:2])):
            yield node[0], node[1]
            return
        if isinstance(node, (list, tuple)):
            for child in node:
                yield from walk(child)
    yield from walk(geom.get("coordinates"))


def rd_bbox(feat):
    """A feature's RD bbox, from the server's own `bbox` or its coordinates."""
    bb = feat.get("bbox")
    if bb and len(bb) >= 4:
        return bb[0], bb[1], bb[2], bb[3]
    xs, ys = [], []
    for x, y in geom_coords(feat.get("geometry")):
        xs.append(x)
        ys.append(y)
    if not xs:
        return None
    return min(xs), min(ys), max(xs), max(ys)


def cells_of_bbox(bb):
    """Every zoom-14 cell an RD bbox touches, as WGS84-derived (x, y) pairs.

    All four corners are reprojected, not two: RD->WGS84 is a rotation as well as
    a scale, so the WGS84 envelope of a rotated rectangle is not the envelope of
    its south-west and north-east corners alone.
    """
    xmin, ymin, xmax, ymax = bb
    lats, lngs = [], []
    for x, y in ((xmin, ymin), (xmax, ymin), (xmin, ymax), (xmax, ymax)):
        lng, lat = _TO_WGS84.transform(x, y)
        lats.append(lat)
        lngs.append(lng)
    # Tile y grows southward, so the north-west corner is (min lng, max lat).
    x0, y0 = lat_lng_to_tile(max(lats), min(lngs))
    x1, y1 = lat_lng_to_tile(min(lats), max(lngs))
    return [(cx, cy) for cx in range(x0, x1 + 1) for cy in range(y0, y1 + 1)]


def point_of(feat):
    """A feature's representative WGS84 (lat, lng) — its bbox centre."""
    bb = rd_bbox(feat)
    if bb is None:
        return None
    lng, lat = _TO_WGS84.transform((bb[0] + bb[2]) / 2.0, (bb[1] + bb[3]) / 2.0)
    return round(lat, 7), round(lng, 7)


def house_number(props):
    """`62` + `A` + `bis` -> `"62Abis"`, the way the address is written."""
    nr = props.get("huisnummer")
    if nr is None:
        return None
    return f"{nr}{props.get('huisletter') or ''}{props.get('toevoeging') or ''}"


class Cells:
    """The per-cell accumulator, with one interned street-name table per cell."""

    def __init__(self):
        self.cells = {}

    def _cell(self, key):
        return self.cells.setdefault(
            key, {"streets": [], "_street_ix": {}, "buildings": [],
                  "addresses": [], "berths": []},
        )

    def street(self, key, name):
        """Intern a street name into the cell's table, returning its index."""
        cell = self._cell(key)
        if not name:
            return None
        ix = cell["_street_ix"].get(name)
        if ix is None:
            ix = len(cell["streets"])
            cell["streets"].append(name)
            cell["_street_ix"][name] = ix
        return ix

    def add(self, key, bucket, record):
        self._cell(key)[bucket].append(record)

    def finish(self):
        """Drop the interning scratch and any cell that ended up empty."""
        out = {}
        for key, cell in self.cells.items():
            cell.pop("_street_ix", None)
            if any(cell[b] for b in ("buildings", "addresses", "berths")):
                out[key] = cell
        return out


def collect_buildings(bbox, cells):
    """`bag:pand` -> one record per building, in every cell its footprint spans."""
    n = 0
    seen = set()
    for feat in pages("bag:pand", bbox):
        props = feat.get("properties") or {}
        pand = props.get("identificatie")
        bb = rd_bbox(feat)
        if not pand or bb is None or pand in seen:
            continue
        seen.add(pand)
        rec = {"id": pand}
        if props.get("bouwjaar"):
            rec["year"] = int(props["bouwjaar"])
        mask = use_mask(props.get("gebruiksdoel"))
        if mask:
            rec["use"] = mask
        # `oppervlakte_min`/`_max` bracket the units inside; equal for the single-
        # unit case, which is most of them, so `area_max` is only worth the bytes
        # when it says something new.
        area = props.get("oppervlakte_min")
        area_max = props.get("oppervlakte_max")
        if area is not None:
            rec["area"] = int(area)
            if area_max is not None and int(area_max) != int(area):
                rec["area_max"] = int(area_max)
        if props.get("aantal_verblijfsobjecten"):
            rec["units"] = int(props["aantal_verblijfsobjecten"])
        status = props.get("status")
        if status and status != STATUS_IN_USE:
            rec["status"] = STATUS_CODES.get(status, status)
        for key in cells_of_bbox(bb):
            cells.add(key, "buildings", rec)
        n += 1
    return n


def collect_addresses(bbox, cells):
    """`bag:verblijfsobject` -> one addressed unit per record, at its own point."""
    n = 0
    seen = set()
    for feat in pages("bag:verblijfsobject", bbox):
        props = feat.get("properties") or {}
        at = point_of(feat)
        vbo = props.get("identificatie")
        if at is None or (vbo and vbo in seen):
            continue
        if vbo:
            seen.add(vbo)
        key = lat_lng_to_tile(at[0], at[1])
        rec = {"at": [at[0], at[1]]}
        if props.get("pandidentificatie"):
            rec["id"] = props["pandidentificatie"]
        ix = cells.street(key, props.get("openbare_ruimte"))
        if ix is not None:
            rec["street"] = ix
        nr = house_number(props)
        if nr:
            rec["nr"] = nr
        if props.get("postcode"):
            rec["postcode"] = props["postcode"]
        mask = use_mask(props.get("gebruiksdoel"))
        if mask:
            rec["use"] = mask
        if props.get("oppervlakte") is not None:
            rec["area"] = int(props["oppervlakte"])
        cells.add(key, "addresses", rec)
        n += 1
    return n


def collect_berths(bbox, cells):
    """`bag:ligplaats` / `bag:standplaats` -> addressed places with no building."""
    n = 0
    seen = set()
    for layer, kind in (("bag:ligplaats", "mooring"), ("bag:standplaats", "plot")):
        for feat in pages(layer, bbox):
            props = feat.get("properties") or {}
            at = point_of(feat)
            oid = props.get("identificatie")
            if at is None or (oid and oid in seen):
                continue
            if oid:
                seen.add(oid)
            key = lat_lng_to_tile(at[0], at[1])
            rec = {"kind": kind, "at": [at[0], at[1]]}
            ix = cells.street(key, props.get("openbare_ruimte"))
            if ix is not None:
                rec["street"] = ix
            nr = house_number(props)
            if nr:
                rec["nr"] = nr
            if props.get("postcode"):
                rec["postcode"] = props["postcode"]
            cells.add(key, "berths", rec)
            n += 1
    return n


def build(bbox, out_dir, name):
    cells = Cells()
    print("panden…", file=sys.stderr)
    n_bld = collect_buildings(bbox, cells)
    print("verblijfsobjecten…", file=sys.stderr)
    n_adr = collect_addresses(bbox, cells)
    print("ligplaatsen + standplaatsen…", file=sys.stderr)
    n_bth = collect_berths(bbox, cells)

    done = cells.finish()
    bdir = os.path.join(out_dir, "bag", str(CACHE_ZOOM))
    for (cx, cy), cell in done.items():
        if cell["buildings"]:
            _write_json(os.path.join(bdir, str(cx), f"{cy}.json"),
                        {"buildings": cell["buildings"]})
        if cell["addresses"] or cell["berths"]:
            _write_json(os.path.join(bdir, str(cx), f"{cy}.places.json"),
                        {"streets": cell["streets"],
                         "addresses": cell["addresses"],
                         "berths": cell["berths"]})
    _write_json(os.path.join(out_dir, "bag", "coverage.json"), {
        "name": name, "zoom": CACHE_ZOOM,
        "attribution": ATTRIBUTION, "source": "Kadaster BAG via PDOK WFS",
        "bbox_rd": list(bbox),
        "cells": sorted([cx, cy] for (cx, cy) in done),
        "building_count": n_bld,
        "address_count": n_adr,
        "berth_count": n_bth,
    })
    print(f"\nDone: {n_bld} buildings, {n_adr} addresses, {n_bth} berths "
          f"across {len(done)} cells.")
    print(f"Output: {out_dir}/bag/{CACHE_ZOOM}/  +  {out_dir}/bag/coverage.json")


def _write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, separators=(",", ":"), ensure_ascii=False)
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(
        description="Extract BAG building attributes + addresses to per-cell files.")
    ap.add_argument("--out", default="site")
    ap.add_argument("--name", default="groningen")
    ap.add_argument("--bbox", type=float, nargs=4,
                    metavar=("XMIN", "YMIN", "XMAX", "YMAX"),
                    default=[229000, 578000, 238000, 587000])
    args = ap.parse_args()
    print(f"BAG attributes '{args.name}' bbox(RD)={args.bbox} -> {args.out}")
    build(tuple(args.bbox), args.out, args.name)


if __name__ == "__main__":
    main()

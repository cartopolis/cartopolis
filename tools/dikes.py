#!/usr/bin/env python3
"""Preprocess the **flood defences** — the country's dikes, dunes and dams — into
per-cell lines the client can name.

This is a country that is largely below the sea, and the map has never once said
so. The relief is already there: a dike is bare earth, so the national height
model `systems::height` fetches contains it, and `systems::terrain` renders it as
a ridge. What is missing is the *knowing* — that the ridge is the Lauwersmeerdijk
and that it is what holds the Wadden Sea out.

The water boards register every defence, and PDOK republishes their registers
together as IMWA (`api.pdok.nl/hwh/waterschappen-keringen-imwa`). The
`waterkering` collection is the one this reads: a line per defence with a name, a
type, and the category that says how much of the country stands behind it.

Measured 2026-08-29 over Groningen, Amsterdam, Utrecht and the Rotterdam delta —
1,320 defences: 936 typed `Dijk`, 244 untyped, 107 `Kunstwerk` (a lock or a
pumping station standing in the line), 29 `Hoge grond`, 3 `Dam`, 1 `Duin`. The
category is filled on all of them; 353 distinct names, nearly all real ones
rather than internal codes. Not every name is a dike's name — a defence is often
called after the water it holds back (`Winschoterdiep`) or the structure it runs
through (`Sluis De Groeve Zuid`) — which is what the register says and so is what
this ships.

The API pages by opaque cursor and reports no total, exactly like the BGT one
next door (see `bgt_objects.py`), so the guard here is the same page ceiling.
Unlike the BGT it does *not* return retired versions: `statusobject` is
`gerealiseerd` on every feature seen, and it is still the filter, because a
planned defence is not one you can stand on.

**Lines are split at cell boundaries, not binned by their first vertex.** The
other pipelines here bin a shape into the cell owning its first point and let
that cell draw it whole, which works because a fence is metres long. A dike is
kilometres long and crosses many cells, so the same rule would hand a 10 km line
to one cell and show it only while that cell was loaded. Each piece here belongs
to exactly one cell, and consecutive pieces **share their boundary vertex** so
they join with no gap — the piece therefore overhangs its cell by at most one
segment, which over these lines is ~18 m.

Wire format per cell — `names` is interned per cell because one defence arrives
as many segments under one name (`Winschoterdiep` 53 times over Groningen);
`lines` is `[type, category, name_index, [[lat, lng], …]]` with `name_index` of
-1 for an unnamed one:

    dikes/{14}/{x}/{y}.json
    {"names": ["Lauwersmeerdijk", …],
     "lines": [[0, 0, 0, [[53.3122, 6.2239], …]], …]}

Data: IMWA Keringen — the Dutch water boards via PDOK, open data.

Usage:
  ./venv/bin/python dikes.py --out site --name groningen \
      --bbox 229000 578000 238000 587000
"""

import argparse
import json
import math
import os
import sys
import tempfile
import time
import urllib.parse
import urllib.request

from pyproj import Transformer

OGC_BASE = "https://api.pdok.nl/hwh/waterschappen-keringen-imwa/ogc/v1"
COLLECTION = "waterkering"
# Features per page. The service accepts 1000 and answers with a `next` cursor.
PAGE = 1_000
# Pages before the walk gives up — the runaway guard for a cursor that stops
# advancing, since there is no total to check a walk against.
MAX_PAGES = 400
CACHE_ZOOM = 14          # must match osm_buildings::CACHE_ZOOM
_TO_WGS84 = Transformer.from_crs("EPSG:28992", "EPSG:4326", always_xy=True)

ATTRIBUTION = "Flood defences © the Dutch water boards (IMWA) via PDOK (open data)"

#: Metres per degree of latitude.
M_PER_DEG = 111_320.0

#: Defence forms, in wire order. Mirrored by `DIKE_KINDS` in `systems::dikes` —
#: **append, never renumber**.
DIKE_KINDS = ["dike", "dune", "dam", "high_ground", "structure", "unknown"]
_KIND_IX = {name: i for i, name in enumerate(DIKE_KINDS)}

#: `typewaterkering` -> our form. The field is empty on about one defence in
#: five, and an untyped one is still a defence with a category and a name, so it
#: maps to `unknown` rather than being dropped: the map has something to say
#: about it either way.
KIND_OF_TYPE = {
    "Dijk": "dike",
    "Duin": "dune",
    "Dam": "dam",
    "Hoge grond": "high_ground",
    # A lock or a pumping station standing in the line. Kept and typed rather
    # than dropped, so the client can decide: it is genuinely part of the
    # defence, and it is genuinely not an embankment.
    "Kunstwerk": "structure",
}

#: `categoriewaterkering`, in wire order — how much country stands behind it.
#: Mirrored by `DIKE_CATEGORIES` in `systems::dikes`.
DIKE_CATEGORIES = ["primary", "regional", "other"]
_CATEGORY_IX = {name: i for i, name in enumerate(DIKE_CATEGORIES)}
CATEGORY_OF = {
    "primair": "primary",
    "regionaal": "regional",
    "overig": "other",
}

#: Simplification tolerance for a defence line, metres.
#:
#: Far coarser than the paving's 10 cm, and it can be: this is a route to be
#: named, not an outline to be filled, and its own vertices arrive ~18 m apart.
#: One metre strips the collinear runs along a straight sea wall and moves
#: nothing the eye can find.
SIMPLIFY_M = 1.0

#: Shortest piece worth keeping, metres. Below this a piece is the stub left
#: where a line clipped a cell corner.
MIN_LENGTH_M = 5.0


def lat_lng_to_tile(lat, lng, z=CACHE_ZOOM):
    n = 2.0 ** z
    x = int((lng + 180.0) / 360.0 * n)
    lat_rad = math.radians(lat)
    y = int((1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi)
            / 2.0 * n)
    return x, y


def wgs84_bbox(rd_bbox):
    """RD bbox -> the WGS84 envelope the OGC API wants (lon/lat, min then max).

    All four corners, not two: RD -> WGS84 turns a rectangle into a quadrilateral,
    and taking the two corners alone clips a sliver off the north and the south.
    """
    x0, y0, x1, y1 = rd_bbox
    corners = [_TO_WGS84.transform(x, y)
               for x in (x0, x1) for y in (y0, y1)]
    lngs = [c[0] for c in corners]
    lats = [c[1] for c in corners]
    return (min(lngs), min(lats), max(lngs), max(lats))


def items(bbox):
    """Every realised defence over a WGS84 bbox, following the `next` cursor."""
    url = (f"{OGC_BASE}/collections/{COLLECTION}/items?"
           + urllib.parse.urlencode({
               "limit": str(PAGE),
               "bbox": ",".join(f"{v:.6f}" for v in bbox),
               "f": "json",
           }))
    seen_pages = kept = total = 0
    while url and seen_pages < MAX_PAGES:
        page = None
        for attempt in range(4):
            try:
                with urllib.request.urlopen(url, timeout=180) as resp:
                    page = json.load(resp)
                break
            except Exception as e:  # noqa: BLE001 - retry any transient failure
                if attempt == 3:
                    print(f"  giving up ({e})", file=sys.stderr)
                    return
                time.sleep(3)
        feats = page.get("features") or []
        if not feats:
            return
        for feat in feats:
            total += 1
            # A planned or removed defence is not one you can stand on.
            if (feat.get("properties") or {}).get("statusobject") != "gerealiseerd":
                continue
            kept += 1
            yield feat
        seen_pages += 1
        print(f"  {kept} realised of {total} seen", file=sys.stderr)
        url = next((link.get("href") for link in page.get("links") or []
                    if link.get("rel") == "next"), None)
    if seen_pages >= MAX_PAGES:
        print(f"  WARNING: hit the {MAX_PAGES}-page guard; this area is "
              f"incomplete.", file=sys.stderr)


def line_strings(geom):
    """A (Multi)LineString as a list of point lists of (lng, lat)."""
    if not geom:
        return []
    t, coords = geom.get("type"), geom.get("coordinates") or []
    if t == "LineString":
        return [coords]
    if t == "MultiLineString":
        return list(coords)
    return []


def simplify(points, tolerance):
    """Douglas-Peucker on a list of (x, y) in metres."""
    if len(points) < 3:
        return list(points)
    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    stack = [(0, len(points) - 1)]
    while stack:
        lo, hi = stack.pop()
        if hi <= lo + 1:
            continue
        ax, ay = points[lo]
        bx, by = points[hi]
        dx, dy = bx - ax, by - ay
        span2 = dx * dx + dy * dy
        worst = tolerance
        pick = None
        for i in range(lo + 1, hi):
            px, py = points[i]
            if span2 == 0.0:
                d = math.hypot(px - ax, py - ay)
            else:
                t = ((px - ax) * dx + (py - ay) * dy) / span2
                t = max(0.0, min(1.0, t))
                d = math.hypot(px - (ax + t * dx), py - (ay + t * dy))
            if d > worst:
                worst, pick = d, i
        if pick is not None:
            keep[pick] = True
            stack.append((lo, pick))
            stack.append((pick, hi))
    return [p for p, k in zip(points, keep) if k]


def length_m(points):
    """Ground length of a (lat, lng) polyline, metres."""
    total = 0.0
    for (lat1, lng1), (lat2, lng2) in zip(points, points[1:]):
        scale = math.cos(math.radians((lat1 + lat2) * 0.5))
        total += math.hypot((lng2 - lng1) * scale, lat2 - lat1) * M_PER_DEG
    return total


def split_by_cell(points):
    """Cut a (lat, lng) polyline into per-cell pieces that join.

    Each piece is `(cell, points)` and every piece **repeats the vertex where the
    cell changed**, so consecutive pieces share an endpoint and the line has no
    gap at a cell edge. The alternative — assigning the whole line to the cell
    owning its first vertex, which is what every other pipeline here does — works
    for a fence and not for a defence that runs across a province.
    """
    out = []
    current = None
    piece = []
    for pt in points:
        cell = lat_lng_to_tile(pt[0], pt[1])
        if current is None:
            current, piece = cell, [pt]
            continue
        if cell != current:
            piece.append(pt)                 # carry the crossing into this piece
            out.append((current, piece))
            current, piece = cell, [piece[-2], pt]  # …and into the next one
            continue
        piece.append(pt)
    if current is not None and len(piece) >= 2:
        out.append((current, piece))
    return out


def build(rd_bbox, out_dir, name):
    bbox = wgs84_bbox(rd_bbox)
    cells = {}
    n_lines = n_pieces = 0
    kinds = {}

    print("keringen…", file=sys.stderr)
    for feat in items(bbox):
        props = feat.get("properties") or {}
        kind = KIND_OF_TYPE.get(props.get("typewaterkering"), "unknown")
        category = CATEGORY_OF.get(props.get("categoriewaterkering"), "other")
        label = (props.get("naam") or "").strip() or None
        for raw in line_strings(feat.get("geometry")):
            pts = [(round(p[1], 7), round(p[0], 7)) for p in raw if len(p) >= 2]
            if len(pts) < 2:
                continue
            n_lines += 1
            kinds[kind] = kinds.get(kind, 0) + 1
            for cell, piece in split_by_cell(pts):
                if length_m(piece) < MIN_LENGTH_M:
                    continue
                # Simplify in local metres, for the reason the paving extractor
                # gives: a tolerance in degrees is a different distance
                # north-south than east-west, by a factor of 1.7 in this country.
                lat0, lng0 = piece[0]
                scale = math.cos(math.radians(lat0))
                local = [((lng - lng0) * scale * M_PER_DEG,
                          (lat - lat0) * M_PER_DEG) for lat, lng in piece]
                kept = simplify(local, SIMPLIFY_M)
                if len(kept) < 2:
                    continue
                back = [[round(lat0 + y / M_PER_DEG, 7),
                         round(lng0 + x / (scale * M_PER_DEG), 7)]
                        for x, y in kept]
                bucket = cells.setdefault(cell, {"names": [], "lines": []})
                if label is None:
                    name_ix = -1
                else:
                    if label not in bucket["names"]:
                        bucket["names"].append(label)
                    name_ix = bucket["names"].index(label)
                bucket["lines"].append(
                    [_KIND_IX[kind], _CATEGORY_IX[category], name_ix, back])
                n_pieces += 1

    ddir = os.path.join(out_dir, "dikes", str(CACHE_ZOOM))
    for (cx, cy), cell in cells.items():
        _write_json(os.path.join(ddir, str(cx), f"{cy}.json"), cell)
    _write_json(os.path.join(out_dir, "dikes", "coverage.json"), {
        "name": name, "zoom": CACHE_ZOOM,
        "attribution": ATTRIBUTION,
        "source": "IMWA Keringen via PDOK OGC API",
        "bbox_rd": list(rd_bbox),
        "kinds": DIKE_KINDS,
        "categories": DIKE_CATEGORIES,
        "cells": sorted([cx, cy] for (cx, cy) in cells),
        "line_count": n_lines,
        "piece_count": n_pieces,
    })
    breakdown = ", ".join(f"{v} {k}" for k, v in sorted(kinds.items()))
    print(f"\nDone: {n_lines} flood defences ({breakdown}) cut into "
          f"{n_pieces} pieces across {len(cells)} cells.")
    print(f"Output: {out_dir}/dikes/{CACHE_ZOOM}/  +  {out_dir}/dikes/coverage.json")


def _write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, separators=(",", ":"), ensure_ascii=False)
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True, help="output root (served tree)")
    ap.add_argument("--name", required=True, help="area name, for the manifest")
    ap.add_argument("--bbox", nargs=4, type=float, required=True,
                    metavar=("XMIN", "YMIN", "XMAX", "YMAX"),
                    help="RD New (EPSG:28992) bounding box")
    args = ap.parse_args()
    build(tuple(args.bbox), args.out, args.name)


if __name__ == "__main__":
    main()

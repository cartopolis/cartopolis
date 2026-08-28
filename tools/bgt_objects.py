#!/usr/bin/env python3
"""Preprocess the **BGT** — the Netherlands' large-scale topography — into the
surveyed street objects the client currently has to invent.

`crates/geo/src/scatter.rs` says the gap out loud: Shortbread's `land` layer
carries *canopy areas, not individual trees*, "so the trees here are scattered,
not surveyed". The BGT surveys them, at ~20 cm, for the whole country — along
with the lampposts, which the map has never had at all. This is that data.

Source is PDOK's BGT OGC API (`api.pdok.nl/lv/bgt/ogc/v1`), which is open and
keyless like the BAG WFS next door but behaves nothing like it: it pages by
opaque **cursor** rather than by index (so there is no 50 000-row wall to split a
bbox around), and it never reports a total. What it does do, and what will ruin a
naive extract, is return **every historical version of every object**: a page of
Groningen tree points is 1000 features of which 485 are current and the rest were
retired years ago. `termination_date` is the discriminator and filtering on it is
not optional — without it the map grows a second, ghost set of trees standing
where trees used to be.

Collections read, and what each becomes on the client:

  * `vegetatieobject_punt` (`plus_type = boom`) — individual trees.
  * `paal` (`lichtmast`) and `straatmeubilair` (`lichtpunt`) — street lighting.
    651 lampposts against 54 light points in one Groningen page: the lamps are
    overwhelmingly *palen*, and a run that reads only the furniture collection
    finds almost none of them.
  * `straatmeubilair` — benches, shelters, picnic tables, artworks, play
    equipment, advertising columns and payment machines, mapped onto the forms
    `crates/geo/src/furniture.rs` can already build.
  * `scheiding_lijn` — fences, walls and noise barriers, as lines the client lays
    panels along.
  * `vegetatieobject_vlak` (`plus_type = haag`) — hedges. Surveyed as areas and
    reduced here to the line down the middle, because a run of boxes laid round a
    hedge's outline draws a hollow rectangle of hedge.

What is deliberately *not* read, and why: `wegdeel` and the terrain parts are the
road and ground surfaces, and taking those would mean replacing the map's own
tessellated geometry rather than adding to it — a different and much larger
change, and one the OSM path would still have to exist alongside for everywhere
that is not the Netherlands. See `docs/notes/surveyed-street-objects.md`.

Wire format per cell — `props` is `[lat, lng, kind]` indexing PROP_KINDS, `lines`
is `[kind, [[lat, lng], …]]` indexing LINE_KINDS:

    bgt/{14}/{x}/{y}.json
    {"trees": [[53.21940, 6.56650], ...],
     "props": [[53.21941, 6.56652, 0], ...],
     "lines": [[0, [[53.2194, 6.5665], [53.2195, 6.5666]]], ...]}

Coordinates stay lat/lng like every other pipeline here; the client turns them
into tile-local metres, which is the frame `furniture.rs` and `scatter.rs`
already work in.

Data: BGT via PDOK — open data, attribution requested.

Usage:
  ./venv/bin/python bgt_objects.py --out site --name groningen \
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

OGC_BASE = "https://api.pdok.nl/lv/bgt/ogc/v1"
# Features per page. The service accepts 1000 and answers with a `next` cursor,
# so this is throughput rather than a limit to work around.
PAGE = 1_000
# Pages per collection before the walk gives up. A city bbox is tens of pages;
# this is the runaway guard for a cursor that stops advancing, which would
# otherwise spin for ever against someone else's server.
MAX_PAGES = 400
CACHE_ZOOM = 14          # must match osm_buildings::CACHE_ZOOM
_TO_WGS84 = Transformer.from_crs("EPSG:28992", "EPSG:4326", always_xy=True)

ATTRIBUTION = "Street objects © Kadaster BGT via PDOK (open data)"

# The client-side forms, in wire order. Mirrored by `PROP_KINDS` in
# `systems::surveyed` — **append, never renumber**.
PROP_KINDS = ["lamp", "bench", "shelter", "picnic_table", "artwork", "swing",
              "cabinet"]
_KIND_IX = {name: i for i, name in enumerate(PROP_KINDS)}

#: Linear forms, in wire order. Mirrored by `LINE_KINDS` in `systems::surveyed`.
LINE_KINDS = ["fence", "wall", "hedge"]
_LINE_IX = {name: i for i, name in enumerate(LINE_KINDS)}

#: `scheiding_lijn.type` -> our linear form.
#:
#: The bank protections — `walbescherming`, `damwand`, `kademuur` — are left out
#: deliberately. They are real and surveyed, and they are retaining structures at
#: the water's edge: drawing them as walls would stand a metre of masonry along
#: every canal, on the side the map already draws as a bank.
LINE_TYPE_FORM = {
    "hek": "fence",
    "draadraster": "fence",
    "muur": "wall",
    "geluidsscherm": "wall",
}

#: How far apart panel instances are laid along a line, metres. Each instance is
#: one box of the same length, so this is also the panel length — a shorter step
#: is a smoother curve and proportionally more objects.
LINE_STEP_M = 2.0

# BGT `plus_type` -> our form. Anything unlisted is dropped rather than given a
# nearest-looking shape: the same "an unstyled polygon beats a wrongly styled
# one" rule the tree scatter and the furniture table already follow. That is why
# `slagboom` (a barrier), `fietsenrek` (a bike rack), `hectometerpaal` and the
# traffic-signal posts are absent — they are real and surveyed, and there is no
# form here that is honestly them.
PLUS_TYPE_FORM = {
    # paal
    "lichtmast": "lamp",
    # straatmeubilair
    "lichtpunt": "lamp",
    "bank": "bench",
    "abri": "shelter",
    "picknicktafel": "picnic_table",
    "kunstobject": "artwork",
    "speelvoorziening": "swing",
    # An advertising column and a payment machine are both a box on a plinth,
    # which is exactly what `FurnitureKind::Cabinet` draws.
    "reclamezuil": "cabinet",
    "betaalautomaat": "cabinet",
}


def lat_lng_to_tile(lat, lng, z=CACHE_ZOOM):
    n = 2.0 ** z
    x = int((lng + 180.0) / 360.0 * n)
    lat_rad = math.radians(lat)
    y = int((1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi)
            / 2.0 * n)
    return x, y


def wgs84_bbox(rd_bbox):
    """RD bbox -> the WGS84 envelope the OGC API wants (lon/lat, min then max).

    All four corners, for the reason `bag_attributes.py` gives: RD->WGS84 turns
    the rectangle as well as scaling it.
    """
    xmin, ymin, xmax, ymax = rd_bbox
    lats, lngs = [], []
    for x, y in ((xmin, ymin), (xmax, ymin), (xmin, ymax), (xmax, ymax)):
        lng, lat = _TO_WGS84.transform(x, y)
        lats.append(lat)
        lngs.append(lng)
    return min(lngs), min(lats), max(lngs), max(lats)


def items(collection, bbox):
    """Every **current** feature of a BGT collection over a WGS84 bbox.

    Follows the `next` cursor and drops every retired version. The retired ones
    are the majority in some collections and they carry a perfectly ordinary
    `status: bestaand`, so `status` is not the test — `termination_date` is.
    """
    url = (f"{OGC_BASE}/collections/{collection}/items?"
           + urllib.parse.urlencode({
               "limit": str(PAGE),
               "bbox": ",".join(f"{v:.6f}" for v in bbox),
               "f": "json",
           }))
    seen_pages = 0
    kept = total = 0
    while url and seen_pages < MAX_PAGES:
        page = None
        for attempt in range(4):
            try:
                with urllib.request.urlopen(url, timeout=180) as resp:
                    page = json.load(resp)
                break
            except Exception as e:  # noqa: BLE001 - retry any transient failure
                if attempt == 3:
                    print(f"  {collection}: giving up ({e})", file=sys.stderr)
                    return
                time.sleep(3)
        feats = page.get("features") or []
        if not feats:
            return
        for feat in feats:
            total += 1
            props = feat.get("properties") or {}
            if props.get("termination_date"):
                continue        # a version that has been superseded or removed
            kept += 1
            yield feat
        seen_pages += 1
        print(f"  {collection}: {kept} current of {total} seen", file=sys.stderr)
        url = next((link.get("href") for link in page.get("links") or []
                    if link.get("rel") == "next"), None)
    if seen_pages >= MAX_PAGES:
        print(f"  WARNING: {collection} hit the {MAX_PAGES}-page guard; this "
              f"area is incomplete.", file=sys.stderr)


def line_points(geom):
    """A (Multi)LineString's vertices as [(lat, lng), …] per part."""
    if not geom:
        return []
    t, coords = geom.get("type"), geom.get("coordinates") or []
    parts = [coords] if t == "LineString" else coords if t == "MultiLineString" else []
    out = []
    for part in parts:
        pts = [(round(p[1], 7), round(p[0], 7)) for p in part if len(p) >= 2]
        if len(pts) >= 2:
            out.append(pts)
    return out


def hedge_centreline(geom):
    """A hedge polygon reduced to the line down its middle.

    A hedge is surveyed as an area and drawn as a run of boxes, so what the client
    needs is a spine, not an outline — laying panels round the outline would draw
    every hedge as a hollow rectangle of hedge.

    The spine is the polygon's principal axis: the direction its points vary in
    most, taken through their centroid and cut at their extremes. That is exactly
    right for the long thin strip a hedge almost always is, and degrades to an
    arbitrary diagonal for the rare blobby one — which is a worse hedge, not a
    wrong place.
    """
    rings = []
    t, coords = geom.get("type"), geom.get("coordinates") or []
    if t == "Polygon":
        rings = coords[:1]
    elif t == "MultiPolygon":
        rings = [poly[0] for poly in coords if poly]
    out = []
    for ring in rings:
        pts = [(p[0], p[1]) for p in ring if len(p) >= 2]
        if len(pts) < 3:
            continue
        cx = sum(p[0] for p in pts) / len(pts)
        cy = sum(p[1] for p in pts) / len(pts)
        # Covariance of the ring about its centroid; the principal axis is the
        # eigenvector of the larger eigenvalue, which for a 2x2 symmetric matrix
        # is one closed-form line rather than a numeric solve.
        sxx = sum((p[0] - cx) ** 2 for p in pts)
        syy = sum((p[1] - cy) ** 2 for p in pts)
        sxy = sum((p[0] - cx) * (p[1] - cy) for p in pts)
        theta = 0.5 * math.atan2(2.0 * sxy, sxx - syy)
        ux, uy = math.cos(theta), math.sin(theta)
        ts = [(p[0] - cx) * ux + (p[1] - cy) * uy for p in pts]
        lo, hi = min(ts), max(ts)
        # In degrees; a hedge shorter than roughly a metre is not worth an object.
        if (hi - lo) < 1.0e-5:
            continue
        a = (round(cy + lo * uy, 7), round(cx + lo * ux, 7))
        b = (round(cy + hi * uy, 7), round(cx + hi * ux, 7))
        out.append([a, b])
    return out


def collect_lines(bbox, cells, cell_of):
    """Fences, walls and hedges, as polylines the client lays panels along."""
    n = 0
    for feat in items("scheiding_lijn", bbox):
        form = LINE_TYPE_FORM.get((feat.get("properties") or {}).get("type"))
        if not form:
            continue
        for pts in line_points(feat.get("geometry")):
            cell_of(pts[0])["lines"].append([_LINE_IX[form], [list(p) for p in pts]])
            n += 1
    for feat in items("vegetatieobject_vlak", bbox):
        if (feat.get("properties") or {}).get("plus_type") != "haag":
            continue
        for pts in hedge_centreline(feat.get("geometry") or {}):
            cell_of(pts[0])["lines"].append([_LINE_IX["hedge"], [list(p) for p in pts]])
            n += 1
    return n


def point_of(feat):
    """A feature's (lat, lng) — its first coordinate, which for these point
    collections is its only one."""
    geom = feat.get("geometry") or {}
    coords = geom.get("coordinates")
    while isinstance(coords, (list, tuple)) and coords and isinstance(coords[0], (list, tuple)):
        coords = coords[0]
    if not coords or len(coords) < 2:
        return None
    lng, lat = coords[0], coords[1]
    return round(lat, 7), round(lng, 7)


def build(rd_bbox, out_dir, name):
    bbox = wgs84_bbox(rd_bbox)
    cells = {}

    def cell_of(at):
        return cells.setdefault(lat_lng_to_tile(*at),
                                {"trees": [], "props": [], "lines": []})

    print("bomen…", file=sys.stderr)
    n_trees = 0
    for feat in items("vegetatieobject_punt", bbox):
        if (feat.get("properties") or {}).get("plus_type") != "boom":
            continue
        at = point_of(feat)
        if at is None:
            continue
        cell_of(at)["trees"].append([at[0], at[1]])
        n_trees += 1

    n_props = 0
    for collection in ("paal", "straatmeubilair"):
        print(f"{collection}…", file=sys.stderr)
        for feat in items(collection, bbox):
            form = PLUS_TYPE_FORM.get((feat.get("properties") or {}).get("plus_type"))
            at = point_of(feat) if form else None
            if at is None:
                continue
            cell_of(at)["props"].append([at[0], at[1], _KIND_IX[form]])
            n_props += 1

    print("hekken, muren en hagen…", file=sys.stderr)
    n_lines = collect_lines(bbox, cells, cell_of)

    bdir = os.path.join(out_dir, "bgt", str(CACHE_ZOOM))
    written = {k: v for k, v in cells.items()
               if v["trees"] or v["props"] or v["lines"]}
    for (cx, cy), cell in written.items():
        _write_json(os.path.join(bdir, str(cx), f"{cy}.json"), cell)
    _write_json(os.path.join(out_dir, "bgt", "coverage.json"), {
        "name": name, "zoom": CACHE_ZOOM,
        "attribution": ATTRIBUTION, "source": "Kadaster BGT via PDOK OGC API",
        "bbox_rd": list(rd_bbox),
        "kinds": PROP_KINDS,
        "line_kinds": LINE_KINDS,
        "cells": sorted([cx, cy] for (cx, cy) in written),
        "tree_count": n_trees,
        "prop_count": n_props,
        "line_count": n_lines,
    })
    print(f"\nDone: {n_trees} surveyed trees, {n_props} street objects, "
          f"{n_lines} fences, walls and hedges across {len(written)} cells.")
    print(f"Output: {out_dir}/bgt/{CACHE_ZOOM}/  +  {out_dir}/bgt/coverage.json")


def _write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, separators=(",", ":"), ensure_ascii=False)
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(
        description="Extract surveyed BGT trees and street objects to per-cell files.")
    ap.add_argument("--out", default="site")
    ap.add_argument("--name", default="groningen")
    ap.add_argument("--bbox", type=float, nargs=4,
                    metavar=("XMIN", "YMIN", "XMAX", "YMAX"),
                    default=[229000, 578000, 238000, 587000])
    args = ap.parse_args()
    print(f"BGT street objects '{args.name}' bbox(RD)={args.bbox} -> {args.out}")
    build(tuple(args.bbox), args.out, args.name)


if __name__ == "__main__":
    main()

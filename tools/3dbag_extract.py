#!/usr/bin/env python3
"""Preprocess 3DBAG building data into per-cell files the viberfox client can
stream, so covered areas (e.g. Groningen) get authoritative BAG heights served
from our own host instead of the slow live API / OSM height guesses.

Pipeline (all server-side, so the client stays in lat/lng + metres):

  1. Page the 3DBAG WFS `BAG3D:lod12` layer over an RD (EPSG:28992) bbox. The
     2D layer gives us a footprint polygon per building plus height attributes
     (`b3_h_maaiveld` ground level, `b3_h_70p`/`b3_h_50p` roof percentiles,
     `b3_h_nok` ridge, `b3_dak_type`, `b3_bouwlagen`) — no CityJSON solid
     decomposition needed. WFS caps at 10 000 features/request; we page with
     `startIndex`.
  2. Reproject every footprint vertex RD (EPSG:28992) -> WGS84 (EPSG:4326).
  3. Turn each building into an **Overpass-shaped way** (`{type,id,geometry,
     tags}`) with `height` / `roof:shape` / `roof:height` tags derived from the
     BAG attributes, so the client's existing `parse_buildings` consumes it
     unchanged — the only client change is *where* it fetches a cell.
  4. Bin each building into the zoom-14 slippy tile of its first vertex (the
     same CACHE_ZOOM cell + first-vertex ownership rule the client uses) and
     write `buildings/14/{x}/{y}.json`, plus a `coverage.json` manifest listing
     the covered cells so the client knows which cells to source from here.

Data: 3DBAG (https://3dbag.nl), 3D geoinformation group, TU Delft — CC BY 4.0;
building footprints from Kadaster BAG.

Usage:
  ./venv/bin/python 3dbag_extract.py --out site \
      --name groningen --bbox 229000 578000 238000 587000
  (bbox is xmin ymin xmax ymax in EPSG:28992 / RD New; default = Groningen city)
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

WFS_BASE = "https://data.3dbag.nl/api/BAG3D/wfs"
WFS_LAYER = "BAG3D:lod12"
PAGE = 10_000            # WFS hard cap per GetFeature request
CACHE_ZOOM = 14         # must match osm_buildings::CACHE_ZOOM
STOREY_M = 3.0          # fallback metres/floor when only bouwlagen is known

# RD New (Amersfoort) -> WGS84 lon/lat. always_xy: input (easting, northing),
# output (lon, lat).
_TO_WGS84 = Transformer.from_crs("EPSG:28992", "EPSG:4326", always_xy=True)

ATTRIBUTION = (
    "3D data © 3DBAG by TU Delft (3D geoinformation), CC BY 4.0; "
    "building footprints © Kadaster BAG"
)


def lat_lng_to_tile(lat, lng, z):
    """Slippy-map tile (x, y) for a lat/lng — identical to viberfox_core::world."""
    n = 2.0 ** z
    x = int((lng + 180.0) / 360.0 * n)
    lat_rad = math.radians(lat)
    y = int((1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi)
            / 2.0 * n)
    return x, y


def wfs_page(bbox, start):
    """Fetch one page of GeoJSON features from the WFS for an RD bbox."""
    xmin, ymin, xmax, ymax = bbox
    params = {
        "service": "WFS",
        "version": "2.0.0",
        "request": "GetFeature",
        "typeNames": WFS_LAYER,
        "count": str(PAGE),
        "startIndex": str(start),
        "outputFormat": "application/json",
        "srsName": "EPSG:28992",
        "bbox": f"{xmin},{ymin},{xmax},{ymax},EPSG:28992",
    }
    url = WFS_BASE + "?" + urllib.parse.urlencode(params)
    for attempt in range(4):
        try:
            with urllib.request.urlopen(url, timeout=120) as resp:
                return json.load(resp)
        except Exception as e:  # noqa: BLE001 - retry any transient failure
            if attempt == 3:
                raise
            print(f"  page {start}: {e} (retry {attempt + 1})", file=sys.stderr)
            time.sleep(3)


def rings_of(geom):
    """Yield each polygon's exterior ring as a list of RD (x, y) tuples.

    Handles Polygon and MultiPolygon; interior rings (holes) are dropped — the
    client extrudes a single outer ring and ignores holes anyway.
    """
    if geom is None:
        return
    t = geom.get("type")
    coords = geom.get("coordinates")
    if t == "Polygon":
        if coords:
            yield [(p[0], p[1]) for p in coords[0]]
    elif t == "MultiPolygon":
        for poly in coords or []:
            if poly:
                yield [(p[0], p[1]) for p in poly[0]]


def _num(props, key):
    v = props.get(key)
    return v if isinstance(v, (int, float)) else None


def height_tags(props):
    """Derive Overpass `height` / `roof:*` tags from BAG height attributes.

    Returns a dict of string tags matching what the client's `parse_buildings`
    expects (`height` = total to ridge, carved from the top; `roof:shape` +
    `roof:height` shape the cap), or None if the building has no usable height.
    """
    ground = _num(props, "b3_h_maaiveld")
    roof = _num(props, "b3_h_70p") or _num(props, "b3_h_50p") or _num(props, "b3_h_max")
    nok = _num(props, "b3_h_nok")
    dak = props.get("b3_dak_type")
    lagen = _num(props, "b3_bouwlagen")

    if ground is None and roof is not None and lagen:
        ground = roof - lagen * STOREY_M  # last-resort ground estimate
    if roof is None and lagen:
        roof = (ground or 0.0) + lagen * STOREY_M
    if roof is None or ground is None:
        return None

    tags = {"building": "yes"}
    if lagen:
        tags["building:levels"] = str(int(lagen))

    if dak == "slanted" and nok is not None and nok > roof:
        # Walls to the LoD1.2 roof level (h_70p), a gable up to the ridge (nok).
        total = max(nok - ground, 2.0)
        roof_h = min(max(nok - roof, 0.5), total * 0.8)
        tags["height"] = f"{total:.2f}"
        tags["roof:shape"] = "gabled"
        tags["roof:height"] = f"{roof_h:.2f}"
    else:
        # Flat extrusion to the roof percentile.
        tags["height"] = f"{max(roof - ground, 2.0):.2f}"
        tags["roof:shape"] = "flat"
    return tags


def feature_id(feat):
    """Stable integer id from the WFS feature id (`lod12.10777377`) or fid."""
    fid = feat.get("id") or ""
    tail = str(fid).rsplit(".", 1)[-1]
    if tail.isdigit():
        return int(tail)
    props = feat.get("properties") or {}
    return int(props.get("fid") or 0)


def build(bbox, out_dir, name):
    cells = {}          # (x, y) -> list[way dict]
    seen = set()
    total_in = total_out = 0
    start = 0
    matched = None

    while True:
        page = wfs_page(bbox, start)
        feats = page.get("features") or []
        if matched is None:
            matched = page.get("numberMatched")
        if not feats:
            break
        for feat in feats:
            total_in += 1
            fid = feature_id(feat)
            tags = height_tags(feat.get("properties") or {})
            if tags is None:
                continue
            for ri, ring in enumerate(rings_of(feat.get("geometry"))):
                if len(ring) < 4:  # ring incl. closing point
                    continue
                # Reproject to WGS84; Overpass geometry is [{lat, lon}, ...].
                geometry = []
                for x, y in ring:
                    lon, lat = _TO_WGS84.transform(x, y)
                    geometry.append({"lat": lat, "lon": lon})
                # One way per polygon; suffix id for MultiPolygon parts.
                wid = fid if ri == 0 else fid * 100 + ri
                if wid in seen:
                    continue
                seen.add(wid)
                # Own the building by its first vertex's z14 cell (matches the
                # client's far-cell ownership rule).
                cx, cy = lat_lng_to_tile(geometry[0]["lat"], geometry[0]["lon"], CACHE_ZOOM)
                cells.setdefault((cx, cy), []).append(
                    {"type": "way", "id": wid, "geometry": geometry, "tags": tags}
                )
                total_out += 1
        got = len(feats)
        print(f"  page start={start}: {got} features "
              f"({total_out} buildings, {len(cells)} cells)", file=sys.stderr)
        start += got
        if matched and start >= matched:
            break
        if got < PAGE:
            break

    # Write per-cell Overpass-shaped files.
    bdir = os.path.join(out_dir, "buildings", str(CACHE_ZOOM))
    for (cx, cy), ways in cells.items():
        cdir = os.path.join(bdir, str(cx))
        os.makedirs(cdir, exist_ok=True)
        _write_json(os.path.join(cdir, f"{cy}.json"), {"elements": ways})

    # Coverage manifest: which z14 cells this host serves, for client routing.
    coverage = {
        "name": name,
        "zoom": CACHE_ZOOM,
        "attribution": ATTRIBUTION,
        "source": "3DBAG WFS BAG3D:lod12",
        "bbox_rd": list(bbox),
        "cells": sorted([cx, cy] for (cx, cy) in cells),
        "building_count": total_out,
    }
    _write_json(os.path.join(out_dir, "coverage.json"), coverage)

    print(f"\nDone: {total_out} buildings from {total_in} features "
          f"across {len(cells)} z{CACHE_ZOOM} cells (matched={matched}).")
    print(f"Output: {out_dir}/buildings/{CACHE_ZOOM}/  +  {out_dir}/coverage.json")


def _write_json(path, obj):
    """Atomic write so a crash mid-write never leaves a half file to be served."""
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, separators=(",", ":"))
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(description="Extract 3DBAG buildings to per-cell files.")
    ap.add_argument("--out", default="site", help="output directory")
    ap.add_argument("--name", default="groningen", help="coverage name")
    ap.add_argument("--bbox", type=float, nargs=4,
                    metavar=("XMIN", "YMIN", "XMAX", "YMAX"),
                    default=[229000, 578000, 238000, 587000],
                    help="RD/EPSG:28992 bbox (default: Groningen city)")
    args = ap.parse_args()
    print(f"3DBAG extract '{args.name}' bbox(RD)={args.bbox} -> {args.out}")
    build(tuple(args.bbox), args.out, args.name)


if __name__ == "__main__":
    main()

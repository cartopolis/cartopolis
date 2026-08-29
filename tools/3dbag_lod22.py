#!/usr/bin/env python3
"""Preprocess 3DBAG **LoD2.2** buildings into per-cell triangle meshes — the real
3D roof geometry (sloped/hipped/complex roofs, dormers), not the LoD1.2 block
extrusion the client would otherwise guess (ADR-027).

Unlike 3dbag_extract.py (which serves 2D footprints + a height for the client to
extrude), this parses the LoD2.2 **Solid** geometry from 3DBAG's CityJSON tiles
and emits ready-to-render triangles, so the client draws actual BAG roofs.

Pipeline (server-side):

  1. Query the 3DBAG `BAG3D:Tiles` WFS for the tiles covering an RD bbox; collect
     each tile's `cj_download` (gzipped CityJSON) URL.
  2. For each CityObject's LoD2.2 Solid: dereference vertices (int * transform.
     scale + translate -> RD x/y/z in metres, z = NAP height), drop the
     GroundSurface (underside, never seen), and triangulate each Wall/Roof
     surface (a planar polygon, possibly with holes) via earcut on its plane.
  3. Reproject each vertex RD(x,y) -> WGS84(lat,lng); height = z - (building's
     min z), so each building sits on the client's flat ground at y=0 like the
     LoD1.2 path. Tag every triangle roof vs wall (from CityJSON semantics) so
     the client colours them with its existing wall/roof palette.
  4. Bin each building into the client's zoom-14 cell (first vertex) and write
     `lod22/{14}/{x}/{y}.json` + `lod22/coverage.json`.

Wire format per cell: {"elements":[{ "id":"0014100010903891", "part":0,
"p":[[lat,lng,h],...], "t":[[i,j,k,roof],...] }, ...]} — `p` deduped vertices,
`t` triangles indexing `p` (roof=1/0). The client projects each vertex through
the same lat/lng->local math it already uses, so no CRS logic moves to the
client. `id` is the **BAG pand identificatie** verbatim, which is what makes
every other Kadaster register (attributes, addresses, monuments) joinable to a
drawn building; `part` is omitted when 0. See `bag_attributes.py`, which keys
its per-cell attribute files on the same string.

Data: 3DBAG (https://3dbag.nl), TU Delft — CC BY 4.0; footprints Kadaster BAG.

Usage:
  ./venv/bin/python 3dbag_lod22.py --out site --name groningen \
      --bbox 229000 578000 238000 587000
"""

import argparse
import gzip
import io
import json
import math
import os
import re
import sys
import tempfile
import urllib.parse
import urllib.request

import numpy as np
import mapbox_earcut as earcut
from pyproj import Transformer

WFS_BASE = "https://data.3dbag.nl/api/BAG3D/wfs"
#: Preferred name of the tile-index layer, and what to fall back to.
#:
#: **Upstream renamed it.** It was `BAG3D:Tiles` when this was written and is
#: `BAG3D:tiles` as of 2026-08-28; asking for the old one earns a flat HTTP 400
#: with `InvalidParameterValue` and nothing that says the name is the problem. So
#: the name is *resolved* against the service's own capabilities rather than
#: hard-coded, and only the case-insensitive match is assumed to be stable.
TILES_LAYER = "BAG3D:tiles"
CACHE_ZOOM = 14
_TO_WGS84 = Transformer.from_crs("EPSG:28992", "EPSG:4326", always_xy=True)

ATTRIBUTION = (
    "3D data © 3DBAG by TU Delft (3D geoinformation), CC BY 4.0; "
    "building footprints © Kadaster BAG"
)


def lat_lng_to_tile(lat, lng, z=CACHE_ZOOM):
    n = 2.0 ** z
    x = int((lng + 180.0) / 360.0 * n)
    lat_rad = math.radians(lat)
    y = int((1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi)
            / 2.0 * n)
    return x, y


def resolve_tiles_layer():
    """The tile-index layer's name as this service currently spells it.

    One extra request at the start of a run, against a rename that costs the whole
    run when it happens and reports itself as a bare 400. Falls back to the
    preferred spelling if the capabilities cannot be read, so a transient failure
    here does not become a hard stop.
    """
    url = WFS_BASE + "?service=WFS&request=GetCapabilities"
    try:
        with urllib.request.urlopen(url, timeout=120) as resp:
            caps = resp.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001
        print(f"  capabilities unreadable ({e}); assuming {TILES_LAYER}",
              file=sys.stderr)
        return TILES_LAYER
    for name in re.findall(r"<Name>([^<]+)</Name>", caps):
        if name.lower() == TILES_LAYER.lower():
            if name != TILES_LAYER:
                print(f"  tile layer is '{name}' upstream, not '{TILES_LAYER}'",
                      file=sys.stderr)
            return name
    print(f"  no tile layer in the capabilities; assuming {TILES_LAYER}",
          file=sys.stderr)
    return TILES_LAYER


def tile_cj_urls(bbox):
    """Every LoD2.2 CityJSON tile download URL intersecting the RD bbox."""
    xmin, ymin, xmax, ymax = bbox
    params = {
        "service": "WFS", "version": "2.0.0", "request": "GetFeature",
        "typeNames": resolve_tiles_layer(), "count": "10000",
        "outputFormat": "application/json", "srsName": "EPSG:28992",
        "bbox": f"{xmin},{ymin},{xmax},{ymax},EPSG:28992",
    }
    url = WFS_BASE + "?" + urllib.parse.urlencode(params)
    with urllib.request.urlopen(url, timeout=120) as resp:
        data = json.load(resp)
    urls = []
    for feat in data.get("features", []):
        cj = (feat.get("properties") or {}).get("cj_download")
        if cj and cj not in urls:
            urls.append(cj)
    return urls


def load_cityjson(url):
    with urllib.request.urlopen(url, timeout=300) as resp:
        raw = resp.read()
    if url.endswith(".gz") or raw[:2] == b"\x1f\x8b":
        raw = gzip.decompress(raw)
    return json.loads(raw)


def triangulate_surface(pts3d, ring_lens):
    """Triangulate one planar surface (outer ring + holes) given as 3D points and
    per-ring lengths. Returns a list of (i, j, k) index triples into pts3d, or []
    if degenerate. Projects to the surface plane, earcuts in 2D."""
    P = np.asarray(pts3d, dtype=np.float64)
    if len(P) < 3:
        return []
    # Newell normal over the outer ring for a robust plane even if non-convex.
    outer = P[:ring_lens[0]]
    n = np.zeros(3)
    for i in range(len(outer)):
        a, b = outer[i], outer[(i + 1) % len(outer)]
        n[0] += (a[1] - b[1]) * (a[2] + b[2])
        n[1] += (a[2] - b[2]) * (a[0] + b[0])
        n[2] += (a[0] - b[0]) * (a[1] + b[1])
    ln = np.linalg.norm(n)
    if ln < 1e-9:
        return []
    n /= ln
    # In-plane basis.
    u = P[1] - P[0]
    u = u - n * np.dot(u, n)
    if np.linalg.norm(u) < 1e-9:
        u = np.cross(n, [1.0, 0.0, 0.0])
        if np.linalg.norm(u) < 1e-9:
            u = np.cross(n, [0.0, 1.0, 0.0])
    u /= np.linalg.norm(u)
    v = np.cross(n, u)
    p2d = np.column_stack(((P - P[0]) @ u, (P - P[0]) @ v)).astype(np.float64)
    rings = np.cumsum(ring_lens).astype(np.uint32)
    try:
        idx = earcut.triangulate_float64(p2d, rings)
    except Exception:
        return []
    return [(int(idx[i]), int(idx[i + 1]), int(idx[i + 2]))
            for i in range(0, len(idx) - 2, 3)]


def real_xyz(vi, verts, scale, translate):
    v = verts[vi]
    return (v[0] * scale[0] + translate[0],
            v[1] * scale[1] + translate[1],
            v[2] * scale[2] + translate[2])


def process_object(geom22, verts, scale, translate):
    """Turn one LoD2.2 Solid into (local_verts[[lat,lng,h]], tris[(i,j,k,roof)]).
    Returns None if it has no usable wall/roof geometry."""
    shell = geom22["boundaries"][0]  # exterior shell: list of surfaces
    sem = geom22.get("semantics") or {}
    sem_vals = (sem.get("values") or [[]])[0]
    sem_surf = sem.get("surfaces") or []

    # Gather the object's vertex indices (for a per-building ground datum).
    used = set()
    for surf in shell:
        for ring in surf:
            used.update(ring)
    if not used:
        return None
    ground_z = min(real_xyz(vi, verts, scale, translate)[2] for vi in used)

    local = {}          # cityjson vertex idx -> local index into `p`
    p = []              # [lat, lng, h]
    tris = []

    def local_of(vi):
        li = local.get(vi)
        if li is None:
            x, y, z = real_xyz(vi, verts, scale, translate)
            lon, lat = _TO_WGS84.transform(x, y)
            li = len(p)
            # Round to ~1 cm (7 dp ≈ 1.1 cm; height to cm) — plenty for building
            # geometry and roughly halves the JSON payload.
            p.append([round(lat, 7), round(lon, 7), round(z - ground_z, 2)])
            local[vi] = li
        return li

    for si, surf in enumerate(shell):
        stype = None
        if si < len(sem_vals) and sem_vals[si] is not None and sem_vals[si] < len(sem_surf):
            stype = sem_surf[sem_vals[si]].get("type")
        if stype == "GroundSurface":
            continue  # underside — never visible on the flat map ground
        is_roof = 1 if stype == "RoofSurface" else 0
        # Surface rings -> 3D pts (outer + holes), remember global idx order.
        pts3d, ring_lens, gidx = [], [], []
        for ring in surf:
            ring_lens.append(len(ring))
            for vi in ring:
                gidx.append(vi)
                pts3d.append(real_xyz(vi, verts, scale, translate))
        for (a, b, c) in triangulate_surface(pts3d, ring_lens):
            tris.append((local_of(gidx[a]), local_of(gidx[b]), local_of(gidx[c]), is_roof))

    if not tris:
        return None
    return p, tris


_PAND_RE = re.compile(r"(?:^|\.)(\d{16})(?:-(\d+))?$")


def object_id(oid):
    """`NL.IMBAG.Pand.0014100010903891-0` -> `("0014100010903891", 0)`.

    The 16-digit body is the BAG **identificatie**: the key BAG, the address
    register, the monuments register and any BIM model of the building are all
    joined on. The `-N` suffix numbers the parts one pand was split into (an
    L-shaped block with two roof forms is one pand and several solids), so it
    belongs beside the id, not inside it.

    This used to keep "the last 15 digits" of everything numeric in the name and
    call the result "stable-ish", which it was — as an internal dedupe key, the
    only thing it was ever used for. It was not the identificatie and could not
    be joined with anything: the part suffix contributes a digit of its own, so
    the truncation shifted the number by a decade for every object carrying one.
    Leading zeros matter too (a `0014…` pand is Groningen), which is why this
    stays a string rather than becoming an int.

    Anything that doesn't parse is passed through verbatim with part 0 — a name
    we don't recognise is still a stable identifier for the object, and inventing
    a hash for it (as the old fallback did, from Python's per-process-salted
    `hash`) makes it differ between two runs of the same build.
    """
    m = _PAND_RE.search(oid)
    if not m:
        return oid, 0
    return m.group(1), int(m.group(2) or 0)


def build(bbox, out_dir, name):
    urls = tile_cj_urls(bbox)
    print(f"{len(urls)} CityJSON tiles cover the bbox", file=sys.stderr)
    cells = {}
    seen = set()
    total = 0
    for ti, url in enumerate(urls):
        try:
            cj = load_cityjson(url)
        except Exception as e:  # noqa: BLE001
            print(f"  tile {url}: {e}", file=sys.stderr)
            continue
        verts = cj["vertices"]
        scale = cj["transform"]["scale"]
        translate = cj["transform"]["translate"]
        n_tile = 0
        for oid, obj in cj.get("CityObjects", {}).items():
            geoms = obj.get("geometry") or []
            g22 = next((g for g in geoms if g.get("lod") == "2.2"
                        and g.get("type") == "Solid"), None)
            if g22 is None:
                continue
            pand, part = object_id(oid)
            if (pand, part) in seen:
                continue
            out = process_object(g22, verts, scale, translate)
            if out is None:
                continue
            seen.add((pand, part))
            p, tris = out
            cx, cy = lat_lng_to_tile(p[0][0], p[0][1])
            el = {"id": pand, "p": p, "t": tris}
            if part:
                el["part"] = part      # omitted for the common single-part pand
            cells.setdefault((cx, cy), []).append(el)
            n_tile += 1
            total += 1
        print(f"  [{ti + 1}/{len(urls)}] {url.split('/')[-1]}: "
              f"{n_tile} buildings ({total} total, {len(cells)} cells)", file=sys.stderr)

    bdir = os.path.join(out_dir, "lod22", str(CACHE_ZOOM))
    for (cx, cy), elems in cells.items():
        _write_json(os.path.join(bdir, str(cx), f"{cy}.json"), {"elements": elems})
    _write_json(os.path.join(out_dir, "lod22", "coverage.json"), {
        "name": name, "zoom": CACHE_ZOOM, "lod": "2.2",
        "attribution": ATTRIBUTION, "source": "3DBAG CityJSON LoD2.2",
        "bbox_rd": list(bbox),
        "cells": sorted([cx, cy] for (cx, cy) in cells),
        "building_count": total,
    })
    print(f"\nDone: {total} LoD2.2 buildings across {len(cells)} cells.")
    print(f"Output: {out_dir}/lod22/{CACHE_ZOOM}/  +  {out_dir}/lod22/coverage.json")


def _write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, separators=(",", ":"))
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(description="Extract 3DBAG LoD2.2 meshes to per-cell files.")
    ap.add_argument("--out", default="site")
    ap.add_argument("--name", default="groningen")
    ap.add_argument("--bbox", type=float, nargs=4,
                    metavar=("XMIN", "YMIN", "XMAX", "YMAX"),
                    default=[229000, 578000, 238000, 587000])
    args = ap.parse_args()
    print(f"3DBAG LoD2.2 '{args.name}' bbox(RD)={args.bbox} -> {args.out}")
    build(tuple(args.bbox), args.out, args.name)


if __name__ == "__main__":
    main()

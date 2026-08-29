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

and the paving beside it, in a file of its own because it is much the heavier
half — `[kind, [ring, …]]` indexing SURFACE_KINDS, with an **optional** third
element indexing SURFACE_MATERIALS:

    bgt/{14}/{x}/{y}.surfaces.json
    {"surfaces": [[0, [[[53.2194, 6.5665], ...]], 2], ...]}

The material is appended rather than inserted so the two-element form still
reads: about one drawn surface in six has nothing said about it, and "it does not
say" has to stay cheap to express and free to draw.

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

#: Metres per degree of latitude, for the local-metre conversions above.
M_PER_DEG = 111_320.0

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

#: Paved-surface forms, in wire order. Mirrored by `SURFACE_KINDS` in
#: `systems::surveyed` — **append, never renumber**.
SURFACE_KINDS = ["footway", "cycleway", "parking", "drive", "verge", "island",
                 "paved", "steps"]
_SURFACE_IX = {name: i for i, name in enumerate(SURFACE_KINDS)}

#: `wegdeel.functie` / `ondersteunendwegdeel.functie` -> our paved form.
#:
#: **The carriageways are deliberately absent.** `rijbaan *`, `spoorbaan` and
#: `OV-baan` stay with OpenStreetMap, which draws them as textured ribbons with
#: lane markings, kerb casings and a per-vertex depth ladder — none of which the
#: BGT can supply, since its own road-marking collection is empty
#: (`weginrichtingselement_*` is `niet-bgt` on every feature). What the BGT adds is
#: everything *beside* the carriageway: the pavement, the cycle path, the parking
#: bay, the verge. OSM draws those as thin constant-width lines or not at all, so
#: the two sources describe different things and cannot fight over the same ground.
SURFACE_FUNCTIE_FORM = {
    # wegdeel
    "voetpad": "footway",
    "voetgangersgebied": "footway",
    "voetpad op trap": "steps",
    "fietspad": "cycleway",
    "ruiterpad": "footway",
    "parkeervlak": "parking",
    "inrit": "drive",
    # A transition is a real piece of paving between two functions — an apron at a
    # junction, the splay of a driveway. It has no function of its own, which is
    # exactly why it gets the neutral form rather than a guess.
    "transitie": "paved",
    # ondersteunendwegdeel
    "berm": "verge",
    "verkeerseiland": "island",
}

#: Surveyed surface materials, in wire order. Mirrored by `SURFACE_MATERIALS` in
#: `systems::surveyed` and by `PavingMaterial` in `crates/geo` — **append, never
#: renumber**.
SURFACE_MATERIALS = ["asphalt", "concrete", "brick", "concrete_paver", "tiles",
                     "stone", "grass_paver", "grass", "planting", "gravel",
                     "sand", "unpaved"]
_MATERIAL_IX = {name: i for i, name in enumerate(SURFACE_MATERIALS)}

#: `plus_fysiek_voorkomen` -> our material. This is the field that names a
#: substance, and on the surfaces this pipeline keeps it is filled far more often
#: than a look at `wegdeel` as a whole suggests: 2,813 of 3,393 (83%) over a
#: square kilometre of central Groningen, 2026-08-29, against about a third when
#: the carriageways are counted in — and the carriageways are exactly what is
#: left to OpenStreetMap here. Anything unlisted is left unknown rather than
#: given a nearest-looking substance, the same rule the furniture table and the
#: tree scatter follow.
#:
#: What that square kilometre turned out to be, by count: 885 concrete pavers,
#: 699 fired brick, 558 tiles, 248 asphalt, 200 decorative stone, 128 concrete,
#: 55 grass, 34 gravel, and 580 with nothing said.
SURFACE_PLUS_MATERIAL = {
    "asfalt": "asphalt",
    "cementbeton": "concrete",
    "beton element": "concrete",
    "gebakken klinkers": "brick",
    "betonstraatstenen": "concrete_paver",
    "tegels": "tiles",
    "sierbestrating": "stone",
    "natuursteen": "stone",
    "grasklinkers": "grass_paver",
    "gras- en kruidachtigen": "grass",
    "bosplantsoen": "planting",
    "heesters": "planting",
    "planten": "planting",
    "boomschors": "planting",
    "grind": "gravel",
    "split": "gravel",
    "schelpen": "gravel",
    "puin": "gravel",
    "zand": "sand",
}

#: `fysiek_voorkomen` -> our material, for the two coarse values that name a
#: *substance*.
#:
#: The other three do not, and reading them would be a guess dressed as data.
#: `open verharding` and `gesloten verharding` describe a **construction** —
#: laid in separate elements, or continuous — and say nothing about what the
#: elements are: over the same three cities `gesloten verharding` resolved to
#: asphalt 94 times and to concrete 59, so calling it either is wrong a third of
#: the time. `half verhard` is worse: its own commonest refinement is
#: `grasklinkers`, which is mostly lawn.
SURFACE_COARSE_MATERIAL = {
    "groenvoorziening": "grass",
    "onverhard": "unpaved",
}


def surface_material(props):
    """The material index for one surface, or `None` where the survey is silent.

    The fine field wins where it is filled; the coarse one only speaks for the
    two values that name a substance.
    """
    name = SURFACE_PLUS_MATERIAL.get(props.get("plus_fysiek_voorkomen"))
    if name is None:
        name = SURFACE_COARSE_MATERIAL.get(props.get("fysiek_voorkomen"))
    return _MATERIAL_IX[name] if name else None


#: Simplification tolerance for a surface ring, metres.
#:
#: **Measured**: the BGT is a 20 cm survey and its rings are correspondingly
#: dense — a page of `ondersteunendwegdeel` over Groningen is 77,202 vertices, of
#: which 4,792 survive a 10 cm Douglas-Peucker. 94% of the data is collinear
#: filler along straight kerbs. Ten centimetres is well under anything visible at
#: any camera distance this layer is drawn at, and without it a city's paving is
#: hundreds of megabytes.
SIMPLIFY_M = 0.10

#: Smallest surface worth drawing, square metres. Below this a polygon is a sliver
#: at a junction that cannot be seen and still costs a triangle.
MIN_AREA_M2 = 1.0

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


def simplify(ring_m, eps):
    """Douglas-Peucker over a ring already in metres. Iterative, not recursive:
    a BGT ring runs to ten thousand points and Python's stack does not."""
    n = len(ring_m)
    if n < 3:
        return ring_m
    keep = [False] * n
    keep[0] = keep[-1] = True
    stack = [(0, n - 1)]
    while stack:
        i, j = stack.pop()
        if j <= i + 1:
            continue
        ax, ay = ring_m[i]
        bx, by = ring_m[j]
        dx, dy = bx - ax, by - ay
        seg = math.hypot(dx, dy)
        best, at = -1.0, -1
        for k in range(i + 1, j):
            px, py = ring_m[k]
            if seg < 1e-12:
                d = math.hypot(px - ax, py - ay)
            else:
                d = abs(dx * (ay - py) - (ax - px) * dy) / seg
            if d > best:
                best, at = d, k
        if best > eps:
            keep[at] = True
            stack.append((i, at))
            stack.append((at, j))
    return [p for p, k in zip(ring_m, keep) if k]


def ring_area_m2(ring_m):
    """Shoelace area of a ring in metres, unsigned."""
    a = 0.0
    for (x1, y1), (x2, y2) in zip(ring_m, ring_m[1:] + ring_m[:1]):
        a += x1 * y2 - x2 * y1
    return abs(a) * 0.5


def polygon_rings(geom):
    """A (Multi)Polygon as a list of polygons, each a list of rings of (lng, lat)."""
    if not geom:
        return []
    t, coords = geom.get("type"), geom.get("coordinates") or []
    if t == "Polygon":
        return [coords]
    if t == "MultiPolygon":
        return list(coords)
    return []


def collect_surfaces(bbox, cells, cell_of):
    """`wegdeel` + `ondersteunendwegdeel` -> the paving beside the carriageway."""
    n = dropped_level = n_material = 0
    for collection in ("wegdeel", "ondersteunendwegdeel"):
        print(f"{collection}…", file=sys.stderr)
        for feat in items(collection, bbox):
            props = feat.get("properties") or {}
            form = SURFACE_FUNCTIE_FORM.get(props.get("functie"))
            if not form:
                continue
            # Ground level only. A `relatieve_hoogteligging` above or below zero is
            # paving on a bridge or in a tunnel, and this layer lies flat on the
            # ground: drawing a viaduct's footway at street level would run a
            # pavement through whatever is underneath it. The bridge decks the map
            # does draw come from the vector tile's own bridge flags.
            if props.get("relatieve_hoogteligging"):
                dropped_level += 1
                continue
            for poly in polygon_rings(feat.get("geometry")):
                out_rings = []
                lat0 = poly[0][0][1] if poly and poly[0] else None
                if lat0 is None:
                    continue
                scale = math.cos(math.radians(lat0))
                for ri, ring in enumerate(poly):
                    # To local metres for both the simplification and the area
                    # test — a tolerance in degrees is a different distance
                    # north-south than east-west, and this is a country where that
                    # ratio is 1.7.
                    m = [((p[0] - poly[0][0][0]) * scale * M_PER_DEG,
                          (p[1] - lat0) * M_PER_DEG) for p in ring]
                    # A GeoJSON ring repeats its first point; simplify the open
                    # form so the closing vertex cannot anchor a corner that is
                    # not there.
                    if len(m) > 1 and m[0] == m[-1]:
                        m = m[:-1]
                    kept = simplify(m, SIMPLIFY_M)
                    if len(kept) < 3:
                        continue
                    if ri == 0 and ring_area_m2(kept) < MIN_AREA_M2:
                        break
                    back = [[round(lat0 + y / M_PER_DEG, 7),
                             round(poly[0][0][0] + x / (scale * M_PER_DEG), 7)]
                            for x, y in kept]
                    out_rings.append(back)
                if not out_rings:
                    continue
                # The material is appended, never inserted: a reader that
                # predates it sees the two-element form it already understood,
                # and a cached cell from before this change still parses.
                entry = [_SURFACE_IX[form], out_rings]
                material = surface_material(props)
                if material is not None:
                    entry.append(material)
                    n_material += 1
                cell_of(tuple(out_rings[0][0]))["surfaces"].append(entry)
                n += 1
    if dropped_level:
        print(f"  {dropped_level} surfaces above or below ground level were "
              f"dropped (bridges and tunnels)", file=sys.stderr)
    if n:
        print(f"  {n_material} of {n} surfaces carry a material "
              f"({100 * n_material // n}%)", file=sys.stderr)
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
                                {"trees": [], "props": [], "lines": [],
                                 "surfaces": []})

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
    n_surf = collect_surfaces(bbox, cells, cell_of)

    bdir = os.path.join(out_dir, "bgt", str(CACHE_ZOOM))
    written = {k: v for k, v in cells.items()
               if v["trees"] or v["props"] or v["lines"] or v["surfaces"]}
    for (cx, cy), cell in written.items():
        # Two files, for the reason `bag_attributes.py` splits its own: the paving
        # is by far the heaviest part and the objects are wanted without it.
        objects = {k: cell[k] for k in ("trees", "props", "lines")}
        if any(objects.values()):
            _write_json(os.path.join(bdir, str(cx), f"{cy}.json"), objects)
        if cell["surfaces"]:
            _write_json(os.path.join(bdir, str(cx), f"{cy}.surfaces.json"),
                        {"surfaces": cell["surfaces"]})
    _write_json(os.path.join(out_dir, "bgt", "coverage.json"), {
        "name": name, "zoom": CACHE_ZOOM,
        "attribution": ATTRIBUTION, "source": "Kadaster BGT via PDOK OGC API",
        "bbox_rd": list(rd_bbox),
        "kinds": PROP_KINDS,
        "line_kinds": LINE_KINDS,
        "surface_kinds": SURFACE_KINDS,
        "surface_materials": SURFACE_MATERIALS,
        "cells": sorted([cx, cy] for (cx, cy) in written),
        "tree_count": n_trees,
        "prop_count": n_props,
        "line_count": n_lines,
        "surface_count": n_surf,
    })
    print(f"\nDone: {n_trees} surveyed trees, {n_props} street objects, "
          f"{n_lines} fences, walls and hedges, {n_surf} paved surfaces "
          f"across {len(written)} cells.")
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

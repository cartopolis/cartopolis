#!/usr/bin/env python3
"""Import an **IFC** building model as a LoD3 element for one BAG building.

This is the rung above `3dbag_lod22.py`. That one streams the real *shell* a
national survey produced — every roof plane, no interior, one solid per building.
An IFC is a building model: walls with thickness, floor slabs, a roof structure,
and rooms between them. Where somebody has one, this puts it on the map in place
of the generalised solid.

**The join is the BAG identificatie**, supplied on the command line. That is the
whole reason `3dbag_lod22.py` was fixed to carry a real one: an IFC model says
nothing about which national register entry it is, so a person has to say it
once — and after that the client can match the model to the mass without any
geometry test at all. Everything downstream of that id already works: the client
looks a building up by it, and `lod22` skips the shell for any building this tree
replaces.

**Georeferencing is an argument, not a discovery.** IFC4 can carry an
`IfcMapConversion` or an `IfcSite` latitude and longitude, and in practice a model
handed over by an architect very often carries neither, or carries a project
origin that is 150 km away in the national grid. Guessing wrongly puts a building
in the wrong street with nothing to say it did, so this asks: `--at lat,lng` is
where the model's own origin goes and `--rotation` turns its +y axis away from
north. `--from-ifc-site` reads the file's own coordinates instead, for the models
that do carry them.

Output is deliberately the **same cell format `lod22.rs` already parses**, with
two fields added that its parser ignores on a shell:

    lod3/{14}/{x}/{y}.json
    {"elements":[{"id":"0014100010909097", "part":0,
                  "class":"IfcWall", "storey":0,
                  "p":[[lat,lng,h],…], "t":[[i,j,k,roof],…]}, …],
     "pands":["0014100010909097"]}

`pands` is the list this cell replaces, so the client can skip those shells
without inspecting every element.

The interior surfaces come through as they are modelled. An IFC wall is a solid
with two faces, so a building imported this way is genuinely hollow — which is
what makes it worth importing rather than flattening to another shell.

Usage:
  ./venv/bin/pip install ifcopenshell
  ./venv/bin/python ifc_lod3.py --ifc model.ifc --out site \\
      --pand 0014100010909097 --at 53.21940,6.56650 --rotation 12.5
"""

import argparse
import json
import math
import os
import sys
import tempfile

import ifcopenshell
import ifcopenshell.geom
import ifcopenshell.util.placement

CACHE_ZOOM = 14          # must match osm_buildings::CACHE_ZOOM
#: Metres per degree of latitude. The building-scale tangent-plane approximation
#: below is good to millimetres over any single structure, which is well inside
#: the tolerance of everything downstream.
M_PER_DEG_LAT = 111_320.0

#: Element classes whose surfaces read as roof rather than wall, for the client's
#: existing per-triangle flag. Everything else is a wall, which is the right
#: default: the flag only chooses between two palette entries.
ROOF_CLASSES = {"IfcRoof"}

#: Classes worth importing at all. An IFC carries furniture, ducting, reinforcement
#: bars and survey points; a map wants the building. Unlisted classes are dropped
#: rather than drawn, which is the same rule the tree scatter and the furniture
#: table follow — and it is what keeps a 400 MB model from becoming a 400 MB tile.
KEEP_CLASSES = {
    "IfcWall", "IfcWallStandardCase", "IfcSlab", "IfcRoof", "IfcColumn",
    "IfcBeam", "IfcStair", "IfcStairFlight", "IfcRailing", "IfcCovering",
    "IfcCurtainWall", "IfcPlate", "IfcMember", "IfcDoor", "IfcWindow",
    "IfcBuildingElementProxy",
}


def lat_lng_to_tile(lat, lng, z=CACHE_ZOOM):
    n = 2.0 ** z
    x = int((lng + 180.0) / 360.0 * n)
    lat_rad = math.radians(lat)
    y = int((1.0 - math.log(math.tan(lat_rad) + 1.0 / math.cos(lat_rad)) / math.pi)
            / 2.0 * n)
    return x, y


def site_lat_lng(model):
    """`IfcSite`'s own coordinates, or None.

    They are stored as (degrees, minutes, seconds, millionths-of-a-second) with
    the sign carried by the *first* component — so a site at 6°33'59"E south of
    the equator has a negative degree field and positive minutes, and summing the
    parts without that sign puts the model on the wrong side of the world.
    """
    for site in model.by_type("IfcSite"):
        lat, lng = site.RefLatitude, site.RefLongitude
        if not lat or not lng:
            continue

        def dms(parts):
            sign = -1.0 if parts[0] < 0 else 1.0
            total = abs(parts[0])
            for i, part in enumerate(parts[1:], start=1):
                total += abs(part) / (60.0 ** i if i < 3 else 3600.0 * 1e6)
            return sign * total

        return dms(lat), dms(lng)
    return None


def storey_index(model):
    """`IfcBuildingStorey` id -> its index from the bottom, by elevation.

    Elevation, not file order: an IFC lists storeys in whatever sequence the
    authoring tool wrote them, and a basement is routinely last.
    """
    storeys = model.by_type("IfcBuildingStorey")
    ordered = sorted(storeys, key=lambda s: (s.Elevation if s.Elevation is not None else 0.0))
    return {s.id(): i for i, s in enumerate(ordered)}


def storey_of(product, index):
    """Which storey an element is contained in, as an index, or None."""
    for rel in getattr(product, "ContainedInStructure", None) or []:
        structure = rel.RelatingStructure
        if structure.is_a("IfcBuildingStorey"):
            return index.get(structure.id())
    return None


def shapes(model):
    """Every keepable product, triangulated, as (product, verts, faces).

    Uses the multi-threaded iterator rather than `create_shape` per product: a
    real model is tens of thousands of elements and the difference is minutes.
    """
    settings = ifcopenshell.geom.settings()
    # **World coordinates, not local.** Without this every element comes back in
    # its own frame with the placement handed over separately in
    # `shape.transformation`, and applying nothing puts the whole building at the
    # project origin: a two-storey model imports as one storey, every wall of
    # every floor stacked on the same spot, and the file *looks* plausible —
    # right element count, right classes, right triangle count, a bounding box a
    # few centimetres across. That is what this flag being off produced the first
    # time, and nothing but a measurement caught it.
    settings.set("use-world-coords", True)
    iterator = ifcopenshell.geom.iterator(settings, model, os.cpu_count() or 1)
    if not iterator.initialize():
        return
    while True:
        shape = iterator.get()
        product = model.by_guid(shape.guid)
        if product.is_a() in KEEP_CLASSES:
            verts = shape.geometry.verts
            faces = shape.geometry.faces
            if verts and faces:
                yield product, verts, faces
        if not iterator.next():
            return


def build(args):
    model = ifcopenshell.open(args.ifc)

    if args.from_ifc_site:
        at = site_lat_lng(model)
        if at is None:
            sys.exit("--from-ifc-site: this model's IfcSite carries no "
                     "RefLatitude/RefLongitude; pass --at instead")
    else:
        at = tuple(float(v) for v in args.at.split(","))
    lat0, lng0 = at
    m_per_deg_lng = M_PER_DEG_LAT * math.cos(math.radians(lat0))
    turn = math.radians(args.rotation)
    cos_t, sin_t = math.cos(turn), math.sin(turn)

    index = storey_index(model)
    elements = []
    ground = None
    collected = []

    for product, verts, faces in shapes(model):
        pts = [(verts[i], verts[i + 1], verts[i + 2]) for i in range(0, len(verts), 3)]
        low = min(p[2] for p in pts)
        ground = low if ground is None else min(ground, low)
        collected.append((product, pts, faces))

    if not collected:
        sys.exit(f"{args.ifc}: nothing importable — no element of "
                 f"{sorted(KEEP_CLASSES)} carries geometry")

    for part, (product, pts, faces) in enumerate(collected):
        cls = product.is_a()
        is_roof = 1 if cls in ROOF_CLASSES else 0
        p = []
        for x, y, z in pts:
            # Turn the model about its own origin, then place it. +x east and +y
            # north at rotation 0, which is the convention the argument's help
            # states and the only one a person can check on a map.
            east = x * cos_t - y * sin_t
            north = x * sin_t + y * cos_t
            p.append([
                round(lat0 + north / M_PER_DEG_LAT, 7),
                round(lng0 + east / m_per_deg_lng, 7),
                round(z - ground, 3),
            ])
        t = [[faces[i], faces[i + 1], faces[i + 2], is_roof]
             for i in range(0, len(faces), 3)]
        element = {"id": args.pand, "part": part, "class": cls, "p": p, "t": t}
        storey = storey_of(product, index)
        if storey is not None:
            element["storey"] = storey
        elements.append(element)

    # Every part of one building goes in the cell that owns the building, not in
    # the cell each part's own first vertex lands in: a model that straddles a
    # cell edge must not be cut in half, because the client draws it as one
    # replacement for one shell.
    cx, cy = lat_lng_to_tile(lat0, lng0)
    path = os.path.join(args.out, "lod3", str(CACHE_ZOOM), str(cx), f"{cy}.json")

    # Merge with whatever is already there, so importing a second building into
    # the same cell does not delete the first.
    existing = {"elements": [], "pands": []}
    if os.path.exists(path):
        with open(path) as f:
            existing = json.load(f)
    kept = [e for e in existing.get("elements", []) if e.get("id") != args.pand]
    pands = sorted({*existing.get("pands", []), args.pand})
    _write_json(path, {"elements": kept + elements, "pands": pands})

    manifest = os.path.join(args.out, "lod3", "coverage.json")
    cells = {(cx, cy)}
    all_pands = set(pands)
    if os.path.exists(manifest):
        with open(manifest) as f:
            old = json.load(f)
        cells |= {(c[0], c[1]) for c in old.get("cells", [])}
        all_pands |= set(old.get("pands", []))
    _write_json(manifest, {
        "name": args.name, "zoom": CACHE_ZOOM, "lod": "3",
        "attribution": args.attribution,
        "source": f"IFC import ({os.path.basename(args.ifc)})",
        "cells": sorted([x, y] for (x, y) in cells),
        "pands": sorted(all_pands),
    })

    tris = sum(len(e["t"]) for e in elements)
    storeys = len({e.get("storey") for e in elements if "storey" in e})
    print(f"imported {len(elements)} elements, {tris} triangles, {storeys} storeys "
          f"for pand {args.pand} into cell {cx}/{cy}")
    print(f"Output: {path}  +  {manifest}")


def _write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w") as f:
        json.dump(obj, f, separators=(",", ":"), ensure_ascii=False)
    os.replace(tmp, path)


def main():
    ap = argparse.ArgumentParser(
        description="Import an IFC building model as a LoD3 element.")
    ap.add_argument("--ifc", required=True, help="path to the .ifc model")
    ap.add_argument("--pand", required=True,
                    help="BAG identificatie of the building this model is")
    ap.add_argument("--at", default="",
                    help="lat,lng the model's origin stands at")
    ap.add_argument("--from-ifc-site", action="store_true",
                    help="take the position from the file's own IfcSite instead")
    ap.add_argument("--rotation", type=float, default=0.0,
                    help="degrees to turn the model's +y axis away from north")
    ap.add_argument("--out", default="site")
    ap.add_argument("--name", default="lod3")
    ap.add_argument("--attribution", default="Building model: source not stated",
                    help="credit line for this model, shown in the client")
    args = ap.parse_args()
    if not args.at and not args.from_ifc_site:
        ap.error("give --at lat,lng or --from-ifc-site")
    build(args)


if __name__ == "__main__":
    main()

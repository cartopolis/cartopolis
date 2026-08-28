#!/usr/bin/env python3
"""Write a small two-storey IFC building, as a fixture for `ifc_lod3.py`.

This exists because a BIM importer with nothing to import is an untested one, and
there is no IFC in this repository — there cannot be, since a real building model
belongs to whoever surveyed it. So the importer is exercised against a model this
script builds: exterior walls, an interior partition, floor slabs and a roof over
two storeys, which is enough geometry to prove the whole path — triangulation,
georeferencing, the storey split and the interior surfaces that make a building
hollow rather than solid.

It is a *fixture*, not a converter: nothing about the real pipeline depends on it,
and a genuine model from a surveyor replaces it by being passed to `ifc_lod3.py`
instead.

Usage:
  ./venv/bin/pip install ifcopenshell
  ./venv/bin/python ifc_demo_building.py --out /tmp/demo.ifc
"""

import argparse

import ifcopenshell
import ifcopenshell.api

#: Footprint of the demo building, metres. Roughly a Dutch canal house: deep,
#: narrow and tall, which is also the shape that makes a storey split visible.
WIDTH = 7.0
DEPTH = 12.0
#: Floor-to-floor height, and how many of them.
STOREY_H = 3.2
STOREYS = 2
WALL_T = 0.25
#: Width of the gap left in the ground-floor front wall.
DOOR_W = 1.4
SLAB_T = 0.25


def run(model, action, **kwargs):
    return ifcopenshell.api.run(action, model, **kwargs)


def wall(model, body, storey, length, height, thickness, at, angle_deg):
    """One wall, placed at `at` (x, y) in metres and turned `angle_deg` about z."""
    import math

    product = run(model, "root.create_entity", ifc_class="IfcWall")
    rep = run(
        model,
        "geometry.add_wall_representation",
        context=body,
        length=length,
        height=height,
        thickness=thickness,
    )
    run(model, "geometry.assign_representation", product=product, representation=rep)
    a = math.radians(angle_deg)
    matrix = [
        [math.cos(a), -math.sin(a), 0.0, at[0]],
        [math.sin(a), math.cos(a), 0.0, at[1]],
        [0.0, 0.0, 1.0, at[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
    run(model, "geometry.edit_object_placement", product=product, matrix=matrix,
        is_si=True)
    run(model, "spatial.assign_container", products=[product],
        relating_structure=storey)
    return product


def slab(model, body, storey, ifc_class, x, y, thickness, at):
    """A floor or roof slab as an extruded rectangle."""
    product = run(model, "root.create_entity", ifc_class=ifc_class)
    profile = model.create_entity(
        "IfcRectangleProfileDef",
        ProfileType="AREA",
        XDim=x,
        YDim=y,
        Position=model.create_entity(
            "IfcAxis2Placement2D",
            Location=model.create_entity("IfcCartesianPoint", Coordinates=(0.0, 0.0)),
        ),
    )
    rep = run(model, "geometry.add_profile_representation", context=body,
              profile=profile, depth=thickness)
    run(model, "geometry.assign_representation", product=product, representation=rep)
    matrix = [
        [1.0, 0.0, 0.0, at[0]],
        [0.0, 1.0, 0.0, at[1]],
        [0.0, 0.0, 1.0, at[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
    run(model, "geometry.edit_object_placement", product=product, matrix=matrix,
        is_si=True)
    run(model, "spatial.assign_container", products=[product],
        relating_structure=storey)
    return product


def build(path):
    model = ifcopenshell.api.run("project.create_file", version="IFC4")
    project = run(model, "root.create_entity", ifc_class="IfcProject",
                  name="Cartopolis demo")
    run(model, "unit.assign_unit")
    ctx = run(model, "context.add_context", context_type="Model")
    body = run(model, "context.add_context", context_type="Model",
               context_identifier="Body", target_view="MODEL_VIEW", parent=ctx)

    site = run(model, "root.create_entity", ifc_class="IfcSite", name="Site")
    building = run(model, "root.create_entity", ifc_class="IfcBuilding",
                   name="Demo building")
    run(model, "aggregate.assign_object", products=[site], relating_object=project)
    run(model, "aggregate.assign_object", products=[building], relating_object=site)

    for level in range(STOREYS):
        z = level * STOREY_H
        storey = run(model, "root.create_entity", ifc_class="IfcBuildingStorey",
                     name=f"Level {level}")
        run(model, "aggregate.assign_object", products=[storey],
            relating_object=building)

        # Four exterior walls around the footprint. `add_wall_representation`
        # builds a wall running along +x from the origin, centred on its
        # thickness, so each is placed at a corner and turned to face along its
        # own side.
        if level == 0:
            # The front wall has a gap in it. A model with no opening imports as a
            # sealed box: correct, and useless as a fixture, because the thing
            # worth checking is that the wall colliders leave the doorway open
            # while still stopping you at the walls.
            side = (WIDTH - DOOR_W) / 2.0
            wall(model, body, storey, side, STOREY_H, WALL_T, (0.0, 0.0, z), 0)
            wall(model, body, storey, side, STOREY_H, WALL_T,
                 (side + DOOR_W, 0.0, z), 0)
        else:
            wall(model, body, storey, WIDTH, STOREY_H, WALL_T, (0.0, 0.0, z), 0)
        wall(model, body, storey, WIDTH, STOREY_H, WALL_T, (0.0, DEPTH, z), 0)
        wall(model, body, storey, DEPTH, STOREY_H, WALL_T, (0.0, 0.0, z), 90)
        wall(model, body, storey, DEPTH, STOREY_H, WALL_T, (WIDTH, 0.0, z), 90)
        # …and one interior partition, which is the whole point of importing a
        # building model rather than a shell: it is a surface that only exists
        # inside.
        wall(model, body, storey, WIDTH, STOREY_H, 0.1, (0.0, DEPTH * 0.55, z), 0)

        slab(model, body, storey, "IfcSlab", WIDTH, DEPTH, SLAB_T,
             (WIDTH / 2.0, DEPTH / 2.0, z - SLAB_T))

    slab(model, body, storey, "IfcRoof", WIDTH, DEPTH, SLAB_T,
         (WIDTH / 2.0, DEPTH / 2.0, STOREYS * STOREY_H))

    model.write(path)
    counts = {c: len(model.by_type(c)) for c in
              ("IfcWall", "IfcSlab", "IfcRoof", "IfcBuildingStorey")}
    print(f"wrote {path}: {counts}")


def main():
    ap = argparse.ArgumentParser(description="Write a demo IFC building.")
    ap.add_argument("--out", default="/tmp/demo.ifc")
    build(ap.parse_args().out)


if __name__ == "__main__":
    main()

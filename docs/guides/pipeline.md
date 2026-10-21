# Preparing map data on a server

`carto-pipeline` and its `.cgnd` ground files, which this guide used to describe,
were removed on 2026-09-19 (`6eddb533`): shipped meshes cannot replace the vector
tile, whose lines the routing, the street names, the traffic, the trees and the
furniture are built from.

The ground is pre-cut on a server again since 2026-10-21, as an addition to the
tile rather than a replacement for it: atlas's `ground` dataset
(`crates/atlas/src/pipeline/ground.rs`) runs the client's own cut,
`cartopolis_geo::ground_sheets::build_sheets`, and the client uses the result only
when it was cut from exactly the inputs the client has.
`docs/notes/pre-cut-ground.md` has the format, the fallbacks and the measurements;
`atlas run --area groningen --datasets ground` runs it, after `bgt`, `roofs`,
`brp` and `terrain`.

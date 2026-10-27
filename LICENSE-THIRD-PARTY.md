# Cartopolis — third-party notices

Cartopolis's own source is `MIT OR Apache-2.0` (see `LICENSE`). This file is the
**canonical list of everything else it is built from**, and several entries are
not optional courtesies: the ODbL, CC BY 4.0 and the SIL Open Font License all
require the notice to travel with anything distributed from the work. So this
file ships in the release tarball, in the Windows zip and inside the APK, and the
in-app About panel renders it. Adding a data source, a model pack or a font means
adding a record here — the README's *Credits* section is prose for a reader, this
is the notice.

Rust dependencies are excluded on purpose: they are enumerated by `Cargo.lock`
and are overwhelmingly `MIT OR Apache-2.0`, so listing them here by hand would be
a copy that rots. The two exceptions below (`Bevy`, `big_space`) are here because
the first is the engine and the second is *vendored into this tree* rather than
resolved from a registry, which is a redistribution of source rather than a
dependency edge.

## Format

Everything from the first `## ` heading below to the end of the file is a list of
records, one per heading, meant to be read by a program as much as by a person:

- `## <name>` opens a record.
- `key: value` — one line each, no continuations, keys are lowercase ASCII.
- `role` what it supplies, `license` the SPDX id or the licence's own name,
  `url` where the licence and the project live, `notice` the exact string a UI
  or a printed credit must show (absent when nothing specific is required), and
  `scope` any limit worth stating (several data sources are Netherlands-only).
- Anything before the first `## ` is prose and can be skipped.

Order is meaningful only in that it groups the engine, then the map data, then
the assets. A renderer may present them in file order.

## Bevy

role: Game engine — ECS, rendering, assets, windowing
license: MIT OR Apache-2.0
url: https://bevy.org/
notice: Built with Bevy.

## big_space

role: Floating-origin transform propagation for planet-scale coordinates
license: MIT OR Apache-2.0
url: https://github.com/aevyrie/big_space
notice: big_space by Aevyrie, MIT OR Apache-2.0. Vendored in crates/big_space as a fork ported to the Bevy release this client tracks.

## OpenStreetMap

role: Map data — roads, buildings, land use, water, places, addresses
license: ODbL 1.0
url: https://www.openstreetmap.org/copyright
notice: © OpenStreetMap contributors, under the Open Database License.

## VersaTiles / Shortbread

role: Vector basemap tiles, in the Shortbread schema, derived from OpenStreetMap
license: ODbL 1.0 (data); the VersaTiles tooling is MIT
url: https://versatiles.org/
notice: Vector tiles from VersaTiles in the Shortbread schema, derived from OpenStreetMap data.

## ESA WorldCover (via VersaTiles landcover-vectors)

role: Land cover under the map — the globe and every far view, where the OSM tiles carry no land use
license: CC BY 4.0
url: https://esa-worldcover.org/en/data-access
notice: © ESA WorldCover project 2021 / Contains modified Copernicus Sentinel data (2021) processed by ESA WorldCover consortium.

## Overpass API

role: Live OSM queries — building footprints and points of interest
license: ODbL 1.0 (the data it serves)
url: https://overpass-api.de/
notice: Building detail via the Overpass API, over OpenStreetMap data.

## Nominatim

role: Place search and reverse geocoding
license: ODbL 1.0 (the data it serves)
url: https://nominatim.org/
notice: Search and reverse geocoding by Nominatim, over OpenStreetMap data.

## 3DBAG

role: LoD2.2 building models
license: CC BY 4.0
url: https://3dbag.nl/
notice: 3DBAG by the 3D geoinformation group, TU Delft, CC BY 4.0 — derived from the Kadaster's BAG registry and AHN, via PDOK.
scope: Netherlands only

## AHN (PDOK)

role: Terrain relief — DTM/DSM height rasters
license: CC0 1.0
url: https://www.ahn.nl/
notice: Elevation from the Actueel Hoogtebestand Nederland, served by PDOK.
scope: Netherlands only

## Luchtfoto (Beeldmateriaal / PDOK)

role: Aerial basemap imagery
license: CC BY 4.0
url: https://www.beeldmateriaal.nl/
notice: Aerial photography by Beeldmateriaal Nederland, served as Luchtfoto RGB (Open) by PDOK, CC BY 4.0.
scope: Netherlands only

## Gemeente Groningen open data

role: Street trees, street lighting, waste containers, charge points and fountains — where the national register is empty
license: CC BY 4.0
url: https://data.groningen.nl/dataset/bomen-gemeente-groningen
notice: Street objects: gemeente Groningen open data (CC BY 4.0).
scope: Gemeente Groningen only

The first entry of a kind that will have more: a *municipal* register, read
because the national one (the BGT, above) leaves its point collections optional
and central Groningen fills none of them. `systems::municipal` holds one entry
per city; each new city is a new heading here, with its own licence, because
these are separate publications by separate bodies and only some of them are CC0.

## ProRail Spoorwegen (PDOK)

role: Level crossings — where they are, and what installation stands on each
license: CC0 1.0
url: https://www.prorail.nl/
notice: Level crossing register by ProRail, served by PDOK.
scope: Netherlands only

## OVapi / NDOV

role: Live public-transport departures and vehicle positions
license: Open data, no key required
url: https://ovapi.nl/
notice: Public transport data from OVapi, over the Dutch NDOV operator feeds.
scope: Netherlands only

## CelesTrak

role: Orbital elements of every active satellite, for the satellite layer
license: Public data (US Space Force catalogue, redistributed by CelesTrak)
url: https://celestrak.org/
notice: Satellite orbits from CelesTrak (celestrak.org).

## MET Norway

role: Weather forecasts driving clouds, fog, rain and wind
license: CC BY 4.0 / NLOD
url: https://api.met.no/
notice: Weather data from the Norwegian Meteorological Institute (MET Norway).

## NOAA GFS

role: Global cloud cover for the planetary sky
license: Public domain (U.S. Government work)
url: https://nomads.ncep.noaa.gov/
notice: Global cloud cover from the NOAA Global Forecast System.

## MakeHuman — base mesh, targets, rig, weights, system assets

role: The avatar bodies (assets/models/avatar/human: the pack and its textures)
license: CC0 1.0 (mpfb2 LICENSE.md section C; each system asset file says "explicitly released as CC0 in September 2020")
url: https://github.com/makehumancommunity/mpfb2 and https://static.makehumancommunity.org/assets/assetpacks/makehuman_system_assets.html
notice: Human base mesh, shape targets, rig, skins, clothes and hair from the MakeHuman project, CC0.

Only the data is used. MPFB's code is GPL-3 and none of it is copied: `crates/human` implements the file formats from their documentation.

## 100STYLE — motion capture

role: The avatar's walk (the Rushed style) and idle (Neutral), cut into the human pack (assets/models/avatar/human)
license: CC BY 4.0
url: https://www.ianxmason.com/100style/ (data: https://zenodo.org/records/8127870)
notice: Walking and idle motion from The 100STYLE Dataset — Ian Mason, CC BY 4.0.

## CMU Graphics Lab Motion Capture Database

role: The avatar's run, gestures and seated pose (run, wave, reach, roll, punch, kick, fall, sit)
license: "free for all uses": may be copied, modified and redistributed, including in products that are sold; the data may not be resold directly, even converted
url: http://mocap.cs.cmu.edu/ (BVH conversion by Bruce Hahne, mirrored at https://github.com/una-dinosauria/cmu-mocap)
notice: The data used in this project was obtained from mocap.cs.cmu.edu. The database was created with funding from NSF EIA-0196217.

Anyone receiving the pack receives these same terms: it may not be resold as motion data.

## HYG Database

role: The naked-eye star catalog behind the night sky (assets/sky/bright_stars.bin)
license: CC BY-SA 4.0
url: https://github.com/astronexus/HYG-Database
notice: Star positions and magnitudes from the HYG Database v4.1 by David Nash, CC BY-SA 4.0 — a compilation of the Hipparcos, Yale Bright Star and Gliese catalogs.

## Freesound and Kenney — city sounds

role: The footsteps and the city's background loops (assets/sounds), cut from field recordings
license: CC0 1.0
url: https://freesound.org and https://kenney.nl/assets/impact-sounds (each recording is listed in assets/sounds/LICENSE.md)
notice: City sounds cut from CC0 recordings by Freesound contributors and Kenney; assets/sounds/LICENSE.md names each one.

## Plus Jakarta Sans

role: The brand font — the boot splash wordmark and the UI's own family
license: SIL Open Font License 1.1
url: https://fonts.google.com/specimen/Plus+Jakarta+Sans
notice: Plus Jakarta Sans by Tokotype, SIL Open Font License 1.1. The font files are redistributed with this software; see assets/fonts/README.md.

## MakeHuman community — ARKit face units

role: The avatar face's 52 live shapes (faceunits01, in the human pack's `face_units`)
license: CC0 1.0 (the asset pack's metadata: "license": "CC0", author Mika Suominen)
url: https://static.makehumancommunity.org/assets/assetpacks.html
notice: Face units for the MakeHuman base mesh by Mika Suominen, CC0.

## MediaPipe Face Landmarker — face detector, landmarks, blend shapes

role: Following the user's own face through the camera to drive their avatar's (assets/models/face, crates/face)
license: Apache-2.0
url: https://ai.google.dev/edge/mediapipe/solutions/vision/face_landmarker (models: https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/latest/face_landmarker.task)
notice: Face detection, face mesh and blend-shape models from Google's MediaPipe, Apache License 2.0.

## tract — neural network inference

role: Runs the face tracker's models on every platform (crates/tract_tflite is tract-tflite 0.23.9 with four operations added)
license: MIT OR Apache-2.0
url: https://github.com/sonos/tract

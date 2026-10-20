//! Ground surfaces: the land, water and site polygons of a tile as one partition of meshes per
//! SurfaceGroup, plus the layer census used to find kinds nothing draws.

use super::*;

/// Which surface a tessellated polygon belongs to. One group = one mesh = one
/// material per tile, which is the whole reason this enum is coarse: batching by
/// group is what keeps the geometry path from spraying a mesh per feature at the
/// GPU (mass mesh churn is what loses the device — see
/// `docs/notes/upload-bursts.md` on `VK_ERROR_DEVICE_LOST`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceGroup {
    Water,
    Forest,
    Grass,
    Sand,
    Built,
    /// A road ribbon. Textured rather than flat-coloured — see
    /// [`road_texture`](crate::road_texture).
    Road(RoadClass),
    /// The pale kerb band under a [`Self::Road`]: the same centre line extruded
    /// [`ROAD_CASING_M`] wider, flat-coloured, half a depth rung below its own
    /// fill so it shows only as an edge. The raster basemap has always cased its
    /// streets; the geometry path drew bare tarmac, so flying *down* into the
    /// vector ring removed cartographic structure instead of adding it — dark
    /// ribbons lying edgeless on the ground.
    RoadCasing(RoadClass),
    /// A bridge deck: the structure's own surface, standing at its measured
    /// height (`systems::height`) rather than lying in the coplanar ground stack.
    /// Above everything else on the ground, because that is what a bridge is.
    BridgeDeck,
    /// Merged tree canopies (`systems::vegetation`). Per-instance tone and luma
    /// variation ride in the mesh's vertex colours, so this stays **one** group,
    /// one material and one mesh per cell like every other group here.
    Canopy,
    /// Merged tree trunks.
    Trunk,
    /// Merged street furniture (`systems::street_furniture`) — benches, bins,
    /// shelters. Same contract as [`Self::Canopy`]: one group, one material, one
    /// mesh per cell, with the per-object variety carried in vertex colours.
    Furniture,
    /// Surveyed paving beside the carriageway (`crate::paving`) — pavements,
    /// cycle paths, parking bays, verges. Same one-group contract again: the
    /// form's tone rides in the vertex colours.
    Paving,
    /// A registered agricultural field, drawn in the tone of the crop grown on it
    /// (`crate::crops`). Same one-group contract: the crop's tone is per vertex.
    ///
    /// It lies *over* [`Self::Grass`] rather than replacing it — the countryside
    /// keeps its generic green everywhere the register does not reach, which is
    /// every country but this one.
    Crop,
    /// A tunnel's carriageway, under the ground rather than on it
    /// ([`crate::tunnels`]). Its own group and not [`Self::Road`], because the
    /// ground road sheet is a *coplanar* stack settled by
    /// `systems::road_depth`'s rung ladder and this sheet is seven metres below
    /// it: putting the two in one mesh would ask that ladder to arbitrate a
    /// contest that does not exist, and would hand `road_stitch` a ribbon it
    /// could cut a surface street out of.
    TunnelRoad(RoadClass),
    /// A tunnel's walls and roof. One group for both: they are the same concrete
    /// and the same material, and splitting them would double the draw calls of
    /// every tunnel to describe a difference nobody can see.
    TunnelStructure,
    /// The retaining wall between a canal's surface and the street beside it,
    /// built by the client from the water it has placed (`map_geometry::quay_walls`)
    /// rather than tessellated from the tile.
    QuayWall,
}

impl SurfaceGroup {
    /// Base colour for the group's material. Lit by the scene's own sun, unlike the
    /// raster path's baked-in shading — this is the point of ADR-035 option B.
    #[must_use]
    pub fn base_color(self) -> [f32; 3] {
        let rgb = match self {
            // Water's colour when the shading toggle is *off*: `map_geometry`
            // builds a `WaterMaterial` only for shaded water, and falls back to
            // this plain branch otherwise. `palette::WATER` is *tuned* to the
            // tone `water::WATER_ALBEDO` shades to, so the two paths land on
            // the same colour and toggling the shading does not step the sea.
            Self::Water => palette::WATER,
            Self::Forest => palette::FOREST,
            Self::Grass => palette::GRASS,
            Self::Sand => palette::SAND,
            Self::Built => palette::RESIDENTIAL,
            // Roads carry their surface in a texture; tinting it would only fight
            // the paint values chosen there, so the material multiplies by white.
            Self::Road(_) => 0xFF_FF_FF,
            // White for the same reason `Canopy`, `Trunk` and `Furniture` are:
            // one material for the whole layer, and the form's own tone carried
            // in the vertex colours (`crate::paving::PavingKind::rgb`).
            Self::Paving => 0xFF_FF_FF,
            // …and the crop's own tone (`crate::crops::rgb`), same contract.
            Self::Crop => 0xFF_FF_FF,
            // A tunnel's carriageway is a carriageway: same texture, same white.
            Self::TunnelRoad(_) => 0xFF_FF_FF,
            Self::TunnelStructure => palette::TUNNEL,
            Self::QuayWall => palette::QUAY_WALL,
            // Same colours the raster path cases with, so the two pipelines'
            // streets keep matching at the ring edge.
            Self::RoadCasing(RoadClass::Motorway | RoadClass::Carriageway) => {
                palette::MOTORWAY_CASING
            }
            Self::RoadCasing(_) => palette::ROAD_CASING,
            Self::BridgeDeck => palette::BRIDGE_DECK,
            // Vertex colours multiply this, and they are centred on 1.0 — so the
            // palette entry stays the one place the canopy tone is written down.
            Self::Canopy => palette::CANOPY,
            Self::Trunk => palette::TRUNK,
            Self::Furniture => palette::FURNITURE,
        };
        [
            ((rgb >> 16) & 0xFF) as f32 / 255.0,
            ((rgb >> 8) & 0xFF) as f32 / 255.0,
            (rgb & 0xFF) as f32 / 255.0,
        ]
    }

    /// Which rung of [`GroundLayer`] — **the** ground stack — this group draws on.
    ///
    /// One exhaustive `match`, and it is the only thing in the crate that decides
    /// where a surface group stands. Everything else about the ordering — the
    /// depth bias, the millimetre height, the room reserved for the road band — is
    /// derived from the layer, so a group cannot be high in one denomination and
    /// low in another. That disagreement is what every recurring z-fight in this
    /// area turned out to be; `ground_stack`'s module docs have the history.
    ///
    /// Being exhaustive is the load-bearing part: a new `SurfaceGroup` does not
    /// compile until somebody says where in the stack it goes.
    #[must_use]
    pub fn layer(self) -> GroundLayer {
        match self {
            Self::Built => GroundLayer::Built,
            Self::Grass => GroundLayer::Grass,
            Self::Crop => GroundLayer::Crop,
            Self::Forest => GroundLayer::Forest,
            Self::Sand => GroundLayer::Sand,
            Self::Water => GroundLayer::Water,
            Self::Paving => GroundLayer::Paving,
            // Both road groups share the band's foot, and that is the point: the
            // road hierarchy — class, then casing, then width — is `road_rung`,
            // carried per *vertex* inside the band, not a second ladder here. It
            // used to be one, denominated in millimetres, and the two crossed over
            // in the middle distance.
            Self::Road(_) | Self::RoadCasing(_) => GroundLayer::Road,
            Self::BridgeDeck => GroundLayer::BridgeDeck,
            // A tunnel takes the standing rung for the same reason a tree does,
            // and the reason is what the rung means rather than what it is called:
            // these are not sheets in the coplanar stack. Every rung below exists
            // to settle a fight between surfaces lying on one plane, and the one
            // thing a tunnel is never coplanar with is the ground over it — it is
            // metres away, which real depth separates on its own. So it wants no
            // height offset (`is_sheet` is false above `Road`, so it gets none)
            // and it is indifferent to the few depth units it inherits.
            Self::TunnelRoad(_) | Self::TunnelStructure => GroundLayer::Standing,
            // Vertical, and never coplanar with anything: the standing rung.
            Self::QuayWall => GroundLayer::Standing,
            Self::Canopy | Self::Trunk | Self::Furniture => GroundLayer::Standing,
        }
    }

    /// Height in metres, relative to the ground plane at y = 0.
    ///
    /// [`GroundLayer::y_offset`] — derived from the same rung as the depth bias,
    /// so the two cannot contradict each other.
    #[must_use]
    pub fn y_offset(self) -> f32 {
        self.layer().y_offset()
    }

    /// Depth-buffer bias, which is what actually stops the flicker at range.
    ///
    /// [`GroundLayer::depth_bias`]. The unit is a depth-buffer ULP, whose apparent
    /// world shift is `1.19e-7 · bias · z` — it grows with range exactly as the
    /// buffer's own quantum does, which is why the ordering neither decays nor
    /// runs away. The millimetre `y_offset`s cannot do that job on their own: a
    /// geometric gap's significance shrinks as 1/z, so climbing collapses it and
    /// only the ladder is left.
    #[must_use]
    pub fn depth_bias(self) -> f32 {
        self.layer().depth_bias()
    }

    /// The group's rung in the ground stack, low to high. Ordering intent only;
    /// [`Self::depth_bias`] turns it into depth-buffer units.
    #[must_use]
    pub fn depth_rung(self) -> f32 {
        self.layer().rung() as f32
    }

    /// Roughness for the group's material. Water is excluded — it has its own
    /// shader (`systems::water`), which owns all of its surface parameters.
    #[must_use]
    pub fn perceptual_roughness(self) -> f32 {
        match self {
            Self::Road(class) => class.perceptual_roughness(),
            _ => 0.95,
        }
    }
}

/// Which `land` kinds map to which surface. Anything unlisted is skipped rather
/// than guessed — an unstyled polygon is better than a wrong one.
pub(crate) fn land_group(kind: &str) -> Option<SurfaceGroup> {
    Some(match kind {
        "forest" | "wood" => SurfaceGroup::Forest,
        // `grassland`, `cemetery`, `playground` and `plant_nursery` were added on
        // 2026-09-10 and were not an oversight so much as an invisible one: an
        // unhandled kind emits no vector surface at all, so the rasterised tile
        // showed through — and the raster's default *is* `GRASS`, so all four
        // already looked like grass and only a walk over one would tell you they
        // grew no blades. Adding them costs nothing at runtime (the same slots,
        // more ground that passes the coverage mask) and is the cheapest "more
        // grass" there is.
        //
        // The list is the kinds observed in real tiles over Groningen, Amsterdam
        // and Utrecht, not the Shortbread schema read off a page: a kind guessed
        // at is a line nothing exercises. `brownfield` is deliberately left out —
        // derelict land is scrubby rather than green, and the raster default
        // already paints it grass, so it stays consistent either way.
        //
        // `wet_meadow`, `marsh`, `greenfield` and `golf_course` were added on
        // 2026-09-19 from the same kind of evidence, gathered the new way: a
        // `carto-view --check` sweep of the 48 z14 cells around Groningen reports
        // every kind the table does not name, so "what are real tiles asking for?"
        // is now a command rather than a thing somebody has to notice. Counts over
        // that sweep: 19, 2, 1 and 1 features.
        //
        // There is no wetland group to put the first two in and inventing one would
        // be a rung, a palette entry and a texture for two features; a wet meadow is
        // a meadow, and the ground it stands on is the thing that was missing.
        //
        // `vineyard`, `miniature_golf`, `grave_yard` and `swamp` came from widening
        // the same sweep past one city — 71 cells over Amsterdam, Utrecht,
        // Rotterdam, Berlin, London, Paris, Barcelona and Oslo. That is the point of
        // the sweep being a command: one city's tiles only ever ask for one city's
        // land.
        //
        // `grave_yard` is the one worth naming. `cemetery` was already here and
        // these are the same ground under two OSM tags — a municipal cemetery and a
        // churchyard — so the map had been drawing one and not the other since the
        // day `cemetery` was added, which no amount of looking at Groningen would
        // have shown.
        "grass" | "meadow" | "park" | "garden" | "farmland" | "orchard" | "scrub" | "heath"
        | "allotments" | "village_green" | "recreation_ground" | "grassland" | "cemetery"
        | "playground" | "plant_nursery" | "wet_meadow" | "marsh" | "greenfield"
        | "golf_course" | "vineyard" | "miniature_golf" | "grave_yard" | "swamp" => {
            SurfaceGroup::Grass
        }
        // `shingle` is a pebble beach, which is what this group already is.
        "sand" | "beach" | "bare_rock" | "scree" | "shingle" => SurfaceGroup::Sand,
        // `railway` is here for the opposite reason to the four above, and it is
        // the one the repaint made urgent: land the table does not name falls
        // through to the raster's `GRASS` default, which at the old saturation
        // was a pale sage nobody queried and is now a vivid green. A railway yard
        // is ballast, not a meadow.
        // `farmyard` (8 features over the Groningen sweep) is here for exactly the
        // reason `railway` is, and is the only kind in that sweep the map was
        // actively drawing *wrong* rather than merely leaving as a hole: it is the
        // hard-surfaced yard a farm's buildings stand around — concrete, gravel,
        // silage clamps — and an unnamed kind falls through to the raster's `GRASS`
        // default, so every farm in the countryside had a meadow for a yard.
        //
        // `greenhouse_horticulture` and `garages` join it from the eight-city sweep,
        // and both are the same mistake as `farmyard`: ground that is roofed or
        // hard-surfaced, falling through to a grass default. The first is the
        // Westland's glasshouse country, which is one of the least green places in
        // the Netherlands and was being drawn as meadow.
        "residential"
        | "industrial"
        | "commercial"
        | "retail"
        | "quarry"
        | "railway"
        | "farmyard"
        | "greenhouse_horticulture"
        | "garages" => SurfaceGroup::Built,
        _ => return None,
    })
}

/// Every layer in a tile, its feature count, and the `kind` values inside it.
///
/// **The answer to "what is actually in this tile?"**, which is the question that
/// keeps coming back: a surface that is missing, a kind nothing draws, a canal
/// rendered in pieces — each was a guess about the data until somebody counted.
/// It reads the tile and nothing else, so it says what is there rather than what
/// this build makes of it.
///
/// Land features this build draws nothing for, by kind and count.
///
/// **An unhandled kind is a hole in the ground**, and a silent one: [`land_group`]
/// answers `None` and the feature is skipped, so the only evidence is ground that
/// nothing covers. The table above is deliberately built from kinds seen in real
/// tiles rather than read off the schema — which makes "what is this tile asking
/// for that we do not draw?" the question that keeps it honest, and this is how it
/// is asked.
///
/// Counts features, not area: one dropped industrial estate matters more than
/// forty dropped kiosks, but the count is what says *which* kind to go and look
/// at, and the coverage measurement already says how much ground is missing.
#[must_use]
/// One street's centre line, in tile-local metres, with what decides how it is
/// drawn.
///
/// The raw input to `tessellate_roads`, handed out so an artifact in a *shape*
/// can be read as coordinates rather than reconstructed from a picture.
pub struct StreetLine {
    pub kind: String,
    pub flags: String,
    pub closed: bool,
    pub points: Vec<[f32; 2]>,
}

/// Every street centre line in the tile, in tile-local metres.
#[must_use]
pub fn street_lines(tile: &TileData, tile_m: f32) -> Vec<StreetLine> {
    let mut out = Vec::new();
    let Some(meta) = tile.layer("streets") else {
        return out;
    };
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    for feature in tile.features(meta) {
        let flags: Vec<&str> = ["bridge", "tunnel", "oneway", "oneway_reverse", "link"]
            .into_iter()
            .filter(|f| flag_of(feature, f))
            .collect();
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for ls in lines {
            let points: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x * scale, c.y * scale])
                    .collect();
            if points.len() < 2 {
                continue;
            }
            let closed = {
                let (h, t) = (points[0], points[points.len() - 1]);
                (h[0] - t[0]).hypot(h[1] - t[1]) < 1e-3
            };
            out.push(StreetLine {
                kind: kind_of(feature).unwrap_or("-").to_string(),
                flags: if flags.is_empty() {
                    String::new()
                } else {
                    format!("[{}]", flags.join("+"))
                },
                closed,
                points,
            });
        }
    }
    out
}

pub fn layer_census(tile: &TileData) -> Vec<(String, usize, Vec<(String, usize)>)> {
    let mut out = Vec::new();
    for meta in tile.layers() {
        let mut kinds: Vec<(String, usize)> = Vec::new();
        let mut total = 0usize;
        for feature in tile.features(meta) {
            total += 1;
            // Flags ride with the kind, because on a `streets` layer they are half
            // of what decides how a feature is drawn — a bridge-flagged rail and a
            // tunnel-flagged one take two different paths out of here, and a
            // census that hides that answers "is this drawn?" with the wrong half
            // of the question.
            let mut kind = kind_of(feature).unwrap_or("-").to_string();
            let flags: Vec<&str> = ["bridge", "tunnel", "oneway", "oneway_reverse", "link"]
                .into_iter()
                .filter(|f| flag_of(feature, f))
                .collect();
            if !flags.is_empty() {
                kind.push('[');
                kind.push_str(&flags.join("+"));
                kind.push(']');
            }
            match kinds.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => kinds.push((kind, 1)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1));
        out.push((meta.name.clone(), total, kinds));
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

pub fn unhandled_land_kinds(tile: &TileData) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let Some(meta) = tile.layer("land") else {
        return out;
    };
    for feature in tile.features(meta) {
        let Some(kind) = kind_of(feature) else {
            continue;
        };
        if land_group(kind).is_some() {
            continue;
        }
        match out.iter_mut().find(|(k, _)| k == kind) {
            Some((_, n)) => *n += 1,
            None => out.push((kind.to_string(), 1)),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// A tessellated surface ready to become a Bevy mesh. Positions are **tile-local
/// metres**: origin at the tile's north-west corner, +x east, +z south — which is
/// the same convention `map_stream` places its ground quads in, so the caller only
/// has to position the tile's corner.
///
/// `PartialEq` compares the vertex data bit for bit, `NaN` included — which is
/// what a round trip through [`crate::ground_file`] has to prove, and is the only
/// thing it is used for. It is not a claim that two meshes drawing the same ground
/// are equal.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceMesh {
    pub group: SurfaceGroup,
    // (see `VERTEX_BYTES` below for what one of these costs on the GPU)
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Texture coordinates, **empty for the flat-coloured area surfaces** (their
    /// material has no texture to sample). Roads fill it: `u` runs 0→1 straight
    /// across the carriageway so the markings register with the geometry, and `v`
    /// is metres travelled along the road over
    /// [`RoadClass::texture_length_m`](crate::road_texture::RoadClass::texture_length_m).
    pub uvs: Vec<[f32; 2]>,
    /// Vertex normals, **empty for the flat ground surfaces** — the caller then
    /// fills in a straight-up normal per vertex. Only the vegetation meshes, which
    /// are the one thing here with actual volume, supply their own.
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA multiplied onto the material's `base_color`, **empty for
    /// everything but vegetation**. This is how thousands of trees get per-instance
    /// tone and brightness variation out of a single shared material: a forest of
    /// one exact green is the giveaway of a procedural scatter.
    pub colors: Vec<[f32; 4]>,
    /// Which rung of the road ladder each vertex draws on, **empty for every group
    /// but [`SurfaceGroup::Road`] and [`SurfaceGroup::RoadCasing`]**.
    ///
    /// This is the *whole* road ordering — class, casing, width band and tie — and
    /// it is deliberately not a height. `systems::road_depth` turns it into a
    /// depth-buffer offset in the vertex stage, which is the only denomination
    /// that settles a coplanar fight at every range; see [`road_rung`] for why the
    /// millimetre ladder it replaced could not.
    pub rungs: Vec<f32>,
}

/// What one vertex of a finished [`SurfaceMesh`] costs on the GPU.
///
/// Position (12) + normal (4) + UV (8) + linear RGBA (16). The normal is four
/// bytes because the client uploads it as `Snorm8x4` (`systems::mesh_format`),
/// not the twelve the struct holds it in here. The normal and UV are present by
/// the time the mesh reaches the GPU **even when the fields above are left
/// empty**, because `map_geometry::build_mesh` fills the gaps — a straight-up
/// normal, a planar UV — and the colour is only uploaded where a group has one,
/// so this is an upper bound that the vegetation groups meet exactly. It was 48
/// while every attribute went up as floats.
///
/// It lives in this crate, which may not depend on bevy, because it is a fact
/// about this struct rather than about the renderer. The *limit* a mesh is cut to
/// is the caller's to supply — that one is a frame budget and belongs to
/// `quality`.
///
/// **Road meshes carry a fifth attribute** — the 4-byte depth rung
/// ([`Self::rungs`](SurfaceMesh::rungs)) — and are still inside this figure,
/// because they are also the group that carries *no* vertex colour: 12 + 4 + 8 +
/// 4 = 28. The constant stays an upper bound over every group, which is what a
/// byte budget needs it to be.
pub const VERTEX_BYTES: usize = 12 + 4 + 8 + 16;
/// Indices budgeted at `u32`. The client uploads `u16` wherever a mesh has few
/// enough vertices, but a planted cell runs well past 65 k, so four is the bound.
pub const INDEX_BYTES: usize = 4;

/// What this many vertices and indices will cost to upload.
#[must_use]
pub fn mesh_upload_bytes(vertices: usize, indices: usize) -> usize {
    vertices * VERTEX_BYTES + indices * INDEX_BYTES
}

/// Cut a finished mesh into pieces of at most `max_bytes` each, by
/// [`mesh_upload_bytes`], keeping every triangle whole and visiting them in
/// curve order, so that each piece is one compact patch the culling can drop.
///
/// **This is the bound that holds after refinement.** The tessellators cut a cell
/// to a byte cap and the terrain refinement then subdivides it, by a factor that
/// depends on the relief: a cap divided by an assumed 2× arrived on the Deck as a
/// single 22.7 MB "Built" mesh against an 8 MB cap, and bevy always uploads one
/// whole asset in one frame, which no per-frame throttle can spread. Cutting the
/// finished mesh is exact whatever the refinement did.
///
/// A piece takes only the vertices its triangles use, remapped, so pieces do not
/// carry each other's attributes. An attribute that was empty stays empty (the
/// caller fills it the same way for every piece). A mesh already under the cap is
/// returned as it is, without a copy. A single triangle is never split, so a
/// `max_bytes` smaller than one triangle still terminates, one triangle a piece.
#[must_use]
pub fn split_by_upload_bytes(mesh: SurfaceMesh, max_bytes: usize) -> Vec<SurfaceMesh> {
    if mesh_upload_bytes(mesh.positions.len(), mesh.indices.len()) <= max_bytes {
        return vec![mesh];
    }
    let n = mesh.positions.len();
    let has = |len: usize| len == n;
    let (has_uv, has_normal, has_color, has_rung) = (
        has(mesh.uvs.len()),
        has(mesh.normals.len()),
        has(mesh.colors.len()),
        has(mesh.rungs.len()),
    );
    let empty = |group| SurfaceMesh {
        group,
        positions: Vec::new(),
        indices: Vec::new(),
        uvs: Vec::new(),
        normals: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    };
    // The piece's index for each source vertex, `u32::MAX` while unused. Reset
    // only for the vertices a piece touched, so a cut costs what the piece held.
    let mut remap = vec![u32::MAX; n];
    let mut touched: Vec<u32> = Vec::new();
    let mut pieces = Vec::new();
    let mut piece = empty(mesh.group);
    // In curve order of the triangles' centres, so each piece is one patch of the
    // cell, and the culling can drop the ones out of view (`crate::patches`).
    let tris: Vec<&[u32]> = mesh.indices.chunks_exact(3).collect();
    let order = crate::patches::spatial_order(&tris, |t| {
        let (a, b, c) = (
            mesh.positions[t[0] as usize],
            mesh.positions[t[1] as usize],
            mesh.positions[t[2] as usize],
        );
        [(a[0] + b[0] + c[0]) / 3.0, (a[2] + b[2] + c[2]) / 3.0]
    });
    for tri in order.into_iter().map(|i| tris[i]) {
        let new_verts = tri
            .iter()
            .filter(|&&v| remap[v as usize] == u32::MAX)
            .count();
        let after = mesh_upload_bytes(piece.positions.len() + new_verts, piece.indices.len() + 3);
        if after > max_bytes && !piece.indices.is_empty() {
            for v in touched.drain(..) {
                remap[v as usize] = u32::MAX;
            }
            pieces.push(std::mem::replace(&mut piece, empty(mesh.group)));
        }
        for &v in tri {
            let slot = &mut remap[v as usize];
            if *slot == u32::MAX {
                let i = v as usize;
                *slot = piece.positions.len() as u32;
                touched.push(v);
                piece.positions.push(mesh.positions[i]);
                if has_uv {
                    piece.uvs.push(mesh.uvs[i]);
                }
                if has_normal {
                    piece.normals.push(mesh.normals[i]);
                }
                if has_color {
                    piece.colors.push(mesh.colors[i]);
                }
                if has_rung {
                    piece.rungs.push(mesh.rungs[i]);
                }
            }
            piece.indices.push(*slot);
        }
    }
    if !piece.indices.is_empty() {
        pieces.push(piece);
    }
    pieces
}

/// Tessellate a tile's area layers into one mesh per [`SurfaceGroup`], **cut so
/// that no two of them cover the same ground**.
///
/// `tile_m` is the tile's ground size in metres, so the output is directly in world
/// units. Only water and landuse are handled here — buildings deliberately are not,
/// because 3DBAG (ADR-027) and the Overpass detail patches (ADR-022) already supply
/// them in 3D, and they are ~75% of a tile's triangles.
///
/// # Why the fills are cut
///
/// [`crate::ground_stack`] orders one layer against another, and that is all a
/// depth ladder can ever do. It cannot order a layer against **itself** — two
/// polygons of one group share a rung by definition — and the tile is full of
/// them: a cemetery inside a park, a playground inside a recreation ground, a
/// village green inside a meadow all arrive as separate `land` features that all
/// resolve to [`SurfaceGroup::Grass`]. Measured on the Groningen z14 fixture
/// before this cut, **9.7 % of the grass and 3.1 % of the built land was covered
/// twice by its own polygons**, and there was nothing anywhere in the tree that
/// could decide which copy to draw.
///
/// That is the same fight, and the same answer, as the road ribbons'
/// ([`crate::road_stitch`], whose module note has the argument in full): a
/// coplanar overlap is not a tie to be broken, it is geometry that should not be
/// there. Each feature keeps only the ground no higher-priority feature claims,
/// and what is drawn is then a partition.
///
/// The cut runs **across groups as well as within them**, which costs nothing
/// extra — the same pass, the same index — and takes the other 10 % out of the
/// ladder's hands too. Measured on the same fixture, a fifth of the woodland lies
/// under grass and nearly a quarter of the water does. The ladder still holds them
/// apart, and now has almost nothing left to hold.
///
/// **Priority, high to low: the layer's rung, then the smaller polygon.** The rung
/// is the map's own order — water over grass, never the other way round. Within a
/// group the smaller feature wins because it is the *more specific* statement
/// about that ground: a cemetery drawn inside a park is a cemetery, and swallowing
/// it into the park is the one outcome nobody wants. The feature index breaks the
/// remaining ties, because the order has to be strict — an order with a draw in it
/// cuts both sides out of the ground they share and opens a hole.
///
/// # What this does *not* let the ladder retire
///
/// The cut is per tile, and a feature reaching across a tile boundary is two
/// features in two tiles that never see each other. The bias is what settles those,
/// as it settles a flyover over the road beneath it. Removing the overlap makes the
/// ladder a backstop instead of the mechanism; it does not make it optional.
///
/// It also means **a group cut out from under another may not be hidden on its
/// own**, or a hole opens where the one above it used to be. No layer toggle
/// reaches these groups today (`DataLayers` has none for water or landuse) and one
/// that did would have to exclude its group from the cut.
/// Half the drawn width of a narrow waterway, by Shortbread `kind`, or `None` for
/// one this build leaves to the basemap.
///
/// **A ditch is deliberately not here.** The Groningen countryside is a lattice of
/// them — thousands of features per cell, each a metre or two wide — and a
/// metre-wide ribbon of water is below the width at which a 3D surface tells you
/// anything a drawn line does not. They stay on the raster basemap underneath,
/// which is where everything this path does not draw already lives.
///
/// The three that are here are the ones a person would call water: a river you
/// cannot jump, a canal a boat uses, a stream with a bank. Widths are the drawn
/// width of the *channel*, not a survey — Shortbread carries no width, so this is
/// the same kind of nominal figure [`road_width_m`] uses for a street.
pub(super) fn water_line_half_width_m(kind: &str) -> Option<f32> {
    Some(
        match kind {
            "river" => 14.0,
            "canal" => 8.0,
            "stream" => 3.0,
            _ => return None,
        } * 0.5,
    )
}

pub fn tessellate_surfaces(
    tile: &TileData,
    tile_m: f32,
    max_bytes: Option<usize>,
) -> Result<Vec<SurfaceMesh>, String> {
    // One entry per *feature*, because the cut needs to tell them apart. They are
    // merged into one mesh per group afterwards, exactly as before.
    let mut fills: Vec<crate::road_stitch::Ribbon> = Vec::new();

    for meta in tile.layers() {
        // (layer, fixed group) — `land` resolves its group per feature instead.
        let fixed = match meta.name.as_str() {
            "ocean" | "water_polygons" => Some(SurfaceGroup::Water),
            "sites" => Some(SurfaceGroup::Built),
            "land" => None,
            // Narrow water is a *line* in Shortbread, not an area, and drawing
            // only the areas is why a canal renders as a row of disconnected
            // ponds: OSM maps the wide stretches as polygons and the narrow ones
            // as ways, and the join between them is invisible on a flat map and
            // a hole in a 3D one. Handled below rather than here, because it
            // needs a ribbon rather than a fill.
            "water_lines" => None,
            _ => continue,
        };
        let is_water_line = meta.name == "water_lines";
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;

        for feature in tile.features(meta) {
            let group = if is_water_line {
                SurfaceGroup::Water
            } else {
                match fixed {
                    Some(g) => g,
                    None => match kind_of(feature).and_then(land_group) {
                        Some(g) => g,
                        None => continue,
                    },
                }
            };
            let mut positions: Vec<[f32; 3]> = Vec::new();
            let mut indices: Vec<u32> = Vec::new();
            if is_water_line {
                let Some(half) = water_line_half_width_m(kind_of(feature).unwrap_or("")) else {
                    continue;
                };
                let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
                match &feature.geometry {
                    Geometry::LineString(ls) => lines.push(ls),
                    Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
                    _ => {}
                }
                let mut uvs: Vec<[f32; 2]> = Vec::new();
                for ls in lines {
                    let pts: Vec<[f32; 2]> =
                        ls.0.iter()
                            .filter(|c| c.x.is_finite() && c.y.is_finite())
                            .map(|c| [c.x, c.y])
                            .collect();
                    // Clipped, not clamped, exactly as the streets are: the
                    // neighbouring tile carries its own copy of the same
                    // waterway and the two must abut rather than overlap.
                    for run in clip_polyline(&pts, 0.0, extent) {
                        let world: Vec<[f32; 2]> =
                            run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                        // Butt ends. A waterway's end is either the tile border
                        // or the polygon it runs into, and `stitch` settles the
                        // overlap with that polygon — an extension would only
                        // give the cut more to remove.
                        let ends = [EndJoin::default(), EndJoin::default()];
                        ribbon(
                            &world,
                            half,
                            0.0,
                            1.0,
                            ends,
                            false,
                            &mut positions,
                            &mut uvs,
                            &mut indices,
                        );
                    }
                }
            } else {
                match &feature.geometry {
                    Geometry::Polygon(p) => {
                        tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
                    }
                    Geometry::MultiPolygon(mp) => {
                        for p in mp {
                            tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
                        }
                    }
                    _ => {}
                }
            }
            if indices.is_empty() {
                continue;
            }
            let fi = fills.len();
            fills.push(crate::road_stitch::Ribbon {
                group,
                rung: 0.0,
                priority: (
                    group.layer().rung(),
                    smaller_first(&positions, &indices),
                    fi,
                    0,
                    0,
                ),
                // Not a bridge, and neither is anything else here — `stitch`'s
                // bridge flag isolates a deck from the roads under it, and a
                // ground fill has no such partner.
                bridge: false,
                positions,
                // Flat-coloured and untextured: the group's material carries the
                // tone (`SurfaceGroup::base_color`), so there is nothing per
                // vertex for the cut to carry across.
                uvs: Vec::new(),
                colors: Vec::new(),
                indices,
            });
        }
    }

    crate::road_stitch::stitch(&mut fills);

    // Back into one mesh per group, cutting a group loose whenever it passes the
    // frame's upload budget — **after** the feature that crossed it rather than
    // before, because a polygon's vertex count is not known until it has been
    // tessellated, so the choice is to overshoot by one feature or to guess.
    // Overshooting by one is bounded; a whole cell's worth is what this exists to
    // stop. A single feature larger than the budget still goes as one mesh, since
    // bevy writes one whole asset per frame regardless.
    let mut out: Vec<SurfaceMesh> = Vec::new();
    let mut builders: Vec<(SurfaceGroup, Vec<[f32; 3]>, Vec<u32>)> = Vec::new();
    for r in fills {
        if r.indices.is_empty() {
            continue;
        }
        let slot = match builders.iter().position(|(g, _, _)| *g == r.group) {
            Some(i) => i,
            None => {
                builders.push((r.group, Vec::new(), Vec::new()));
                builders.len() - 1
            }
        };
        let (_, positions, indices) = &mut builders[slot];
        let base = positions.len() as u32;
        positions.extend(r.positions);
        indices.extend(r.indices.iter().map(|i| i + base));
        if max_bytes.is_some_and(|max| mesh_upload_bytes(positions.len(), indices.len()) >= max) {
            out.push(SurfaceMesh {
                group: r.group,
                positions: std::mem::take(positions),
                indices: std::mem::take(indices),
                uvs: Vec::new(),
                normals: Vec::new(),
                colors: Vec::new(),
                rungs: Vec::new(),
            });
        }
    }
    out.extend(
        builders
            .into_iter()
            .filter(|(_, p, i)| !p.is_empty() && !i.is_empty())
            .map(|(group, positions, indices)| SurfaceMesh {
                group,
                positions,
                indices,
                uvs: Vec::new(),
                normals: Vec::new(),
                colors: Vec::new(),
                rungs: Vec::new(),
            }),
    );
    Ok(out)
}

/// What fraction of a cell the given surfaces actually cover, sampled on an
/// `n × n` grid.
///
/// **A hole in the ground is otherwise only found by pointing a camera at it**,
/// and that is not a test — twice on 2026-09-19 a hole was measured from a
/// viewpoint that happened to include, or happened to miss, the block it was in.
/// This is per cell, deterministic, and free of any opinion about where to look.
///
/// It will never reach 100 %: the cut runs on a millimetre lattice and this samples
/// texel centres, so a sliver survives along every kerb. What it is for is the
/// difference between that residual and a *block* of missing ground.
///
/// Sampling rather than area: exact polygon area would have to resolve the overlaps
/// the cut is supposed to have removed, and would report 103 % if it had not.
#[must_use]
pub fn surface_coverage(meshes: &[SurfaceMesh], tile_m: f32, n: usize) -> f32 {
    if n == 0 || tile_m <= 0.0 {
        return 0.0;
    }
    let mut hit = vec![false; n * n];
    #[allow(clippy::cast_precision_loss)]
    let scale = n as f32 / tile_m;
    for m in meshes {
        for t in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = t
                .iter()
                .map(|&i| {
                    let v = m.positions[i as usize];
                    [v[0] * scale, v[2] * scale]
                })
                .collect();
            let (a, b, c) = (p[0], p[1], p[2]);
            // Both ends clamped: a tile carries a buffer, so a ribbon can lie
            // entirely past the grid and a `min` on the maximum alone walks off the
            // end of the row.
            let lo = |v: f32| -> usize {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let i = v.floor().max(0.0) as usize;
                i.min(n - 1)
            };
            let hi = |v: f32| -> usize {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let i = v.ceil().max(0.0) as usize;
                i.min(n - 1)
            };
            let (minx, maxx) = (lo(a[0].min(b[0]).min(c[0])), hi(a[0].max(b[0]).max(c[0])));
            let (miny, maxy) = (lo(a[1].min(b[1]).min(c[1])), hi(a[1].max(b[1]).max(c[1])));
            let side = |u: [f32; 2], v: [f32; 2], q: [f32; 2]| {
                (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0])
            };
            for y in miny..=maxy {
                for x in minx..=maxx {
                    #[allow(clippy::cast_precision_loss)]
                    let q = [x as f32 + 0.5, y as f32 + 0.5];
                    let (s0, s1, s2) = (side(a, b, q), side(b, c, q), side(c, a, q));
                    if (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                        || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                    {
                        hit[y * n + x] = true;
                    }
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    {
        hit.iter().filter(|h| **h).count() as f32 / (n * n) as f32
    }
}

/// A feature's rank by size, **descending** — bigger area, smaller number — so it
/// can go straight into `road_stitch`'s "larger priority wins" ordering and mean
/// "the smaller polygon draws on top".
///
/// Twice the triangle area, summed and saturated into a `u32` of square metres.
/// Quantising to the square metre is deliberate: two features of genuinely equal
/// size then tie here and fall through to the feature index, which is stable,
/// rather than being separated by a float's last bit — the same reason
/// `road_stitch::Ribbon::priority` spells its tie-breaks out.
fn smaller_first(positions: &[[f32; 3]], indices: &[u32]) -> u32 {
    let mut twice_area = 0.0f32;
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        );
        twice_area += ((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs();
    }
    let sq_m = (twice_area * 0.5) as u32;
    u32::MAX - sq_m.min(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::tessellate_surfaces;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// A strip of `quads` unit squares, two triangles each, with every attribute
    /// filled, so a split has something to carry and something to check.
    fn strip(quads: usize) -> SurfaceMesh {
        let mut m = SurfaceMesh {
            group: SurfaceGroup::Grass,
            positions: Vec::new(),
            indices: Vec::new(),
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        };
        for i in 0..=quads {
            for z in 0..2 {
                let p = [i as f32, 0.0, z as f32];
                m.positions.push(p);
                m.uvs.push([p[0], p[2]]);
                m.normals.push([0.0, 1.0, 0.0]);
                m.colors.push([p[0], p[2], 0.5, 1.0]);
                m.rungs.push(p[0]);
            }
        }
        for i in 0..quads as u32 {
            let (a, b, c, d) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
            m.indices.extend([a, b, c, b, d, c]);
        }
        m
    }

    /// Every piece fits the cap, and together the pieces draw exactly the same
    /// triangles — same corners, same attributes at each corner, each triangle's
    /// corners in the same order. The triangles themselves come out in curve order.
    #[test]
    fn a_split_mesh_fits_the_cap_and_draws_the_same_triangles() {
        let whole = strip(500);
        let cap = mesh_upload_bytes(60, 90);
        let pieces = split_by_upload_bytes(whole.clone(), cap);
        assert!(pieces.len() > 1, "a mesh over the cap must be cut");
        let corner = |m: &SurfaceMesh, i: u32| {
            let i = i as usize;
            let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
            [
                bits(&m.positions[i]),
                bits(&m.uvs[i]),
                bits(&m.normals[i]),
                bits(&m.colors[i]),
                bits(&[m.rungs[i]]),
            ]
        };
        let triangles = |m: &SurfaceMesh| -> Vec<Vec<_>> {
            m.indices
                .chunks_exact(3)
                .map(|t| t.iter().map(|&i| corner(m, i)).collect())
                .collect()
        };
        let mut drawn = Vec::new();
        for p in &pieces {
            assert!(mesh_upload_bytes(p.positions.len(), p.indices.len()) <= cap);
            assert_eq!(p.group, whole.group);
            for len in [p.uvs.len(), p.normals.len(), p.colors.len(), p.rungs.len()] {
                assert_eq!(len, p.positions.len(), "an attribute fell out of step");
            }
            drawn.extend(triangles(p));
        }
        let mut want = triangles(&whole);
        drawn.sort();
        want.sort();
        assert_eq!(drawn, want);
    }

    /// **The pieces are patches, not stripes.** Each one is culled as a whole by its
    /// box, so a piece that spans the cell is drawn whenever any of it is in view.
    #[test]
    fn a_split_mesh_is_cut_into_compact_patches() {
        // A 256 m square of 1 m quads, listed row by row: cut in source order
        // every piece would be a full-width band.
        let n = 256u32;
        let mut m = SurfaceMesh {
            group: SurfaceGroup::Grass,
            positions: Vec::new(),
            indices: Vec::new(),
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        };
        for j in 0..=n {
            for i in 0..=n {
                m.positions.push([i as f32, 0.0, j as f32]);
            }
        }
        for j in 0..n {
            for i in 0..n {
                let a = j * (n + 1) + i;
                let (b, c, d) = (a + 1, a + n + 1, a + n + 2);
                m.indices.extend([a, b, c, b, d, c]);
            }
        }
        let pieces = split_by_upload_bytes(m, mesh_upload_bytes(4_000, 12_000));
        assert!(pieces.len() >= 8, "{} pieces", pieces.len());
        for p in &pieces {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for v in &p.positions {
                lo = [lo[0].min(v[0]), lo[1].min(v[2])];
                hi = [hi[0].max(v[0]), hi[1].max(v[2])];
            }
            let wide = (hi[0] - lo[0]).max(hi[1] - lo[1]);
            assert!(wide <= 128.0, "a piece spans {wide} m of a 256 m square");
        }
    }

    /// Under the cap nothing is copied or cut, and an empty attribute stays empty
    /// in every piece rather than being invented.
    #[test]
    fn a_mesh_under_the_cap_is_returned_whole_and_empty_attributes_stay_empty() {
        let small = strip(4);
        let cap = mesh_upload_bytes(small.positions.len(), small.indices.len());
        assert_eq!(split_by_upload_bytes(small.clone(), cap), vec![small]);

        let mut bare = strip(200);
        bare.uvs.clear();
        bare.colors.clear();
        let pieces = split_by_upload_bytes(bare, mesh_upload_bytes(30, 45));
        assert!(pieces.len() > 1);
        assert!(pieces
            .iter()
            .all(|p| p.uvs.is_empty() && p.colors.is_empty()));
        assert!(pieces.iter().all(|p| p.normals.len() == p.positions.len()));
    }

    /// A cap below one triangle still ends, one triangle a piece, rather than
    /// looping or dropping geometry.
    #[test]
    fn a_cap_below_one_triangle_still_terminates() {
        let pieces = split_by_upload_bytes(strip(3), 1);
        assert_eq!(pieces.len(), 6);
        assert!(pieces.iter().all(|p| p.indices.len() == 3));
    }

    #[test]
    fn tessellates_water_and_landuse() {
        let tile_m = 1000.0;
        let meshes = tessellate_surfaces(GRONINGEN_Z14, tile_m).expect("tessellate");
        assert!(!meshes.is_empty(), "no surfaces tessellated");

        let water = meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::Water)
            .expect("Groningen has canals");
        assert!(water.indices.len() >= 3);
        assert_eq!(
            water.indices.len() % 3,
            0,
            "indices must be whole triangles"
        );

        for m in &meshes {
            let n = m.positions.len() as u32;
            assert!(
                m.indices.iter().all(|&i| i < n),
                "{:?}: index out of range",
                m.group
            );
            // Tile-local metres, inside the tile, and flat on the ground.
            //
            // **A ribbon is allowed half its width past the edge**, and only a
            // ribbon. A fill is clipped to the tile exactly (`clip_ring`), so it
            // abuts its neighbour's copy on the border line itself. A narrow
            // waterway is a *line* clipped to that same border and then widened,
            // so its cap reaches half a channel beyond — which is what the street
            // ribbons have always done, for the same reason and to the same
            // extent. The allowance is the widest water line this build draws.
            let slack = if m.group == SurfaceGroup::Water {
                water_line_half_width_m("river").unwrap_or(0.0)
            } else {
                0.0
            };
            for p in &m.positions {
                assert!(
                    (-slack..=tile_m + slack).contains(&p[0]),
                    "{:?}: x {} out of tile",
                    m.group,
                    p[0]
                );
                assert!(
                    (-slack..=tile_m + slack).contains(&p[2]),
                    "{:?}: z {} out of tile",
                    m.group,
                    p[2]
                );
                assert_eq!(p[1], 0.0);
            }
            // Every triangle wound to face up, so the ground is not backface-culled.
            for t in m.indices.chunks_exact(3) {
                let (a, b, c) = (
                    m.positions[t[0] as usize],
                    m.positions[t[1] as usize],
                    m.positions[t[2] as usize],
                );
                let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
                assert!(cross <= 0.0, "{:?}: triangle wound downward", m.group);
            }
        }
    }

    /// **No surface group may cover the same ground twice.** This is the half of
    /// the z-fight story `ground_stack` explicitly cannot reach: two polygons of
    /// one group share a rung by definition, so a layer fighting itself has no
    /// tie-break anywhere in the tree.
    ///
    /// It is not hypothetical. Measured on this fixture before
    /// `tessellate_surfaces` started cutting, on 2026-09-15: **9.7 % of the grass
    /// and 3.1 % of the built land** was covered twice by its own polygons — parks
    /// with cemeteries in them, recreation grounds with playgrounds in them, every
    /// one a separate `land` feature resolving to the same group. And the
    /// 2026-08-29 ghost pavements were the same shape of bug in the surveyed
    /// layer, found on a screen a fortnight after they shipped.
    ///
    /// The ceiling is not zero, and the reason is the tile buffer: a feature
    /// clipped to the tile plus a buffer can reach past its own edge, and the cut
    /// works on the millimetre lattice, so a sliver of a texel survives here and
    /// there. It is well under a percent, which is the difference between a
    /// residual and a layer.
    #[test]
    fn no_group_overlaps_itself() {
        const CEILING: f32 = 1.0;
        for (group, covered, twice) in group_coverage() {
            if covered == 0 {
                continue;
            }
            let pct = 100.0 * twice as f32 / covered as f32;
            assert!(
                pct <= CEILING,
                "{group:?} covers {twice} of its own {covered} texels twice ({pct:.1} %) — \
                 no depth rung can order a layer against itself"
            );
        }
    }

    /// …and no two groups share ground either, which takes the other half out of
    /// the ladder's hands.
    ///
    /// Measured before the cut: a fifth of the woodland lay under grass, nearly a
    /// quarter of the water over it, and a tenth of the built land under it. The
    /// ladder still orders what is left — a flyover over the road beneath it, and
    /// anything that crosses a tile boundary, which is two features in two tiles
    /// that never see each other — so this is a backstop becoming a backstop, not
    /// a ladder becoming unnecessary.
    #[test]
    fn no_two_groups_share_ground() {
        const CEILING: f32 = 1.0;
        let masks = group_masks();
        for (i, (ga, a)) in masks.iter().enumerate() {
            for (gb, b) in masks.iter().skip(i + 1) {
                let both = a.iter().zip(b.iter()).filter(|(x, y)| **x && **y).count();
                let area = a.iter().filter(|x| **x).count().max(1);
                let pct = 100.0 * both as f32 / area as f32;
                assert!(
                    pct <= CEILING,
                    "{ga:?} and {gb:?} share {both} texels ({pct:.1} % of {ga:?})"
                );
            }
        }
    }

    /// One coverage count per group: texels its own triangles cover at all, and
    /// texels they cover more than once.
    fn group_coverage() -> Vec<(SurfaceGroup, usize, usize)> {
        const N: usize = 1024;
        let meshes = bytes::tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let mut counts: Vec<(SurfaceGroup, Vec<u16>)> = Vec::new();
        for m in &meshes {
            let slot = match counts.iter().position(|(g, _)| *g == m.group) {
                Some(i) => i,
                None => {
                    counts.push((m.group, vec![0u16; N * N]));
                    counts.len() - 1
                }
            };
            rasterise(
                m,
                N,
                |cell| *cell = cell.saturating_add(1),
                &mut counts[slot].1,
            );
        }
        counts
            .into_iter()
            .map(|(g, c)| {
                (
                    g,
                    c.iter().filter(|v| **v >= 1).count(),
                    c.iter().filter(|v| **v >= 2).count(),
                )
            })
            .collect()
    }

    /// The same, as a plain covered/not-covered mask per group.
    fn group_masks() -> Vec<(SurfaceGroup, Vec<bool>)> {
        const N: usize = 1024;
        let meshes = bytes::tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let mut masks: Vec<(SurfaceGroup, Vec<bool>)> = Vec::new();
        for m in &meshes {
            let slot = match masks.iter().position(|(g, _)| *g == m.group) {
                Some(i) => i,
                None => {
                    masks.push((m.group, vec![false; N * N]));
                    masks.len() - 1
                }
            };
            rasterise(m, N, |cell| *cell = true, &mut masks[slot].1);
        }
        masks
    }

    /// **And no ground fill may lie under a carriageway either.** This was the
    /// largest overlap left anywhere in the ground stack and the last one to be
    /// removed.
    ///
    /// It is a different pair from the two above: those are fills against fills,
    /// settled inside [`tessellate_surfaces`]. The roads are built by a separate
    /// pass that needs the bridge decks first, so a fill and a ribbon had never
    /// met — and a landuse polygon covers the street through it, because that is
    /// what a residential block *is*. Measured on this fixture before
    /// [`crate::road_stitch::cut_under_roads`] was given the fills, 2026-09-19:
    ///
    /// | | of the road sheet's area |
    /// |---|---|
    /// | built land under road | **67.1 %** |
    /// | grass under road | 7.2 % |
    /// | woodland under road | 1.7 % |
    ///
    /// Three quarters of every carriageway in the city had a second opaque
    /// coplanar surface beneath it, held apart by half a millimetre of
    /// `GroundLayer` clearance. That works while the ground is flat and stops the
    /// moment it has a gradient: each sheet is bent onto the measured ground
    /// against its own triangles, so the two disagree by the field's curvature
    /// across an edge — decimetres on a bank, up to 2.8 m on a handset — and the
    /// lower one stands through the upper one in jagged polygons that follow
    /// triangle edges. That is the phone report of 2026-09-19.
    ///
    /// The ceiling is the same sub-one-percent residual the two tests above allow,
    /// and for the same reason: the cut runs on a millimetre lattice and this
    /// samples texel centres, so a sliver survives along every kerb.
    #[test]
    fn no_ground_fill_lies_under_a_road() {
        const N: usize = 1024;
        const CEILING: f32 = 1.0;
        let roads = bytes::tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).unwrap();
        let mut road = vec![false; N * N];
        for m in &roads {
            rasterise(m, N, |cell| *cell = true, &mut road);
        }

        let mut fills = bytes::tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let before = shared_with(&fills, &road, N);
        assert!(
            before > 0,
            "the fixture no longer has a fill under a road — this test is measuring nothing"
        );

        crate::road_stitch::cut_under_roads(&mut fills, &roads);
        let after = shared_with(&fills, &road, N);
        let pct = 100.0 * after as f32 / before as f32;
        assert!(
            pct <= CEILING,
            "{after} of {before} texels under a carriageway still carry a ground fill \
             ({pct:.1} %) — no depth rung can order two sheets on measured ground"
        );
    }

    /// Texels covered both by one of `meshes` and by the road mask.
    fn shared_with(meshes: &[SurfaceMesh], road: &[bool], n: usize) -> usize {
        let mut mask = vec![false; n * n];
        for m in meshes {
            rasterise(m, n, |cell| *cell = true, &mut mask);
        }
        mask.iter().zip(road).filter(|(a, b)| **a && **b).count()
    }

    /// Scan-convert a surface mesh onto an `N × N` grid over the tile, calling
    /// `mark` on each covered cell. Centre sampling and no anti-aliasing: this
    /// measures *coverage*, and a half-covered texel counted as covered by both of
    /// two neighbours would read as an overlap that is not there.
    fn rasterise<T>(m: &SurfaceMesh, n: usize, mark: impl Fn(&mut T), grid: &mut [T]) {
        for t in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = t
                .iter()
                .map(|&i| {
                    let v = m.positions[i as usize];
                    [v[0] / 1000.0 * n as f32, v[2] / 1000.0 * n as f32]
                })
                .collect();
            let (a, b, c) = (p[0], p[1], p[2]);
            // Both ends clamped, not just the far one: a tile carries a buffer, so a
            // ribbon can start past the grid's far edge entirely and a `min` that only
            // bounded the maximum left the loop indexing off the end.
            let minx = (a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize).min(n - 1);
            let maxx = ((a[0].max(b[0]).max(c[0]).ceil()).max(0.0) as usize).min(n - 1);
            let miny = (a[1].min(b[1]).min(c[1]).floor().max(0.0) as usize).min(n - 1);
            let maxy = ((a[1].max(b[1]).max(c[1]).ceil()).max(0.0) as usize).min(n - 1);
            let side = |u: [f32; 2], v: [f32; 2], q: [f32; 2]| {
                (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0])
            };
            for y in miny..=maxy.max(miny) {
                for x in minx..=maxx.max(minx) {
                    let q = [x as f32 + 0.5, y as f32 + 0.5];
                    let (s0, s1, s2) = (side(a, b, q), side(b, c, q), side(c, a, q));
                    if (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                        || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                    {
                        mark(&mut grid[y * n + x]);
                    }
                }
            }
        }
    }

    #[test]
    fn buildings_are_not_tessellated() {
        // 3DBAG and the Overpass detail patches already draw these in 3D, and they
        // are ~75% of a tile's triangles — pulling them in here would double the
        // geometry for no gain.
        let meshes = tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let tris: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
        assert!(
            tris < 12_000,
            "{tris} triangles — buildings likely leaked into the surface pass"
        );
    }
}

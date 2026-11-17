//! **A lens's areas, drawn by the GPU** (`systems::scenario` is the lens).
//!
//! Until 2026-11-17 the areas were egui shapes: every frame, every area in
//! view was projected vertex by vertex through `place_pins::place_delta`, its
//! outline a second time, and the lot handed to egui to tessellate — at a
//! buurt's height hundreds of areas of hundreds of points each, all on the CPU,
//! which is the main thread on the phone. A lens that was on made the whole map
//! slow to move.
//!
//! Now each board is meshed **once per frame of reference** on a worker: its
//! areas triangulated, laid where `place_delta` would put them — on the flat
//! map's curved frame below the space tier, on the sphere above it — and cut
//! into a few chunks so the GPU culls what is out of view. Every vertex
//! carries the texel of its area's colour (UV0), so what changes as a year
//! plays, a figure is switched or a band is singled out is one texel an area
//! ([`LensAreaMaterial::colours`]), eased here, and nothing else is uploaded
//! while the map moves. The edges are a line list on the same material, one
//! texel past the areas holding their ink, each border once.
//!
//! The material draws over the map with the depth test off, as the overlay
//! was, and drops what lies past the horizon in its shader. The board is laid
//! at the ground's curve, not on the relief: with the depth test off that only
//! shifts a fill a little against a hillside seen at an angle, and the relief
//! the client has reaches 3.2 km from the anchor.

use crate::components::FreeCamera;
use crate::platform::sync::{Arc, Mutex};
use crate::resources::MapAnchor;
use crate::systems::globe::GlobeState;
use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use big_space::prelude::{BigSpace, GridCell};
use cartopolis_core::geo::TangentFrame;
use cartopolis_materials::lens::LensAreaMaterial;
use cartopolis_scenario::board::AreaSet;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Colour texels per row of a board's texture.
const ROW: u32 = 256;
/// How far behind the horizon a point is still drawn, as `place_delta` allows:
/// at eye level in a polder the camera is below sea level.
const HORIZON_MARGIN_M: f32 = 200.0;
/// The flat map's reach, as `place_pins::MAX_FLAT_RANGE_M`: an area further
/// from the anchor is left off the flat board.
const MAX_FLAT_RANGE_M: f64 = 250_000.0;
/// How fast a drawn colour closes on its state's: most of the way in a fifth
/// of a second.
const EASE_PER_S: f32 = 12.0;
/// A board is cut into about this many chunks a side, so the GPU can cull.
const CHUNKS_ACROSS: f64 = 6.0;

/// Where a board's areas are laid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lay {
    /// On the flat map, curved down with the Earth (`GlobeState::drop_at`).
    Flat,
    /// On the sphere, for the space tier.
    Sphere,
}

/// What laying a board needs, copied for a worker.
#[derive(Clone, Copy, Debug)]
pub struct Reference {
    pub lay: Lay,
    pub anchor: (f64, f64, u32),
    pub globe_lat: f64,
    pub globe_lng: f64,
    pub centre: Vec3,
    pub radius: f32,
    pub revision: u32,
}

impl Reference {
    /// One number for "the same frame and the same lay".
    fn key(&self) -> (Lay, u32, u64, u64, u32) {
        (
            self.lay,
            self.revision,
            self.anchor.0.to_bits(),
            self.anchor.1.to_bits(),
            self.anchor.2,
        )
    }

    /// A point of the board in the anchor's frame, and the ground's up there;
    /// `None` off the flat map's reach.
    fn place(&self, frame: Option<&TangentFrame>, lng: f64, lat: f64) -> Option<(Vec3, Vec3)> {
        let p = match (self.lay, frame) {
            (Lay::Sphere, Some(f)) => crate::systems::globe::geodetic_to_local(f, lat, lng, 0.0),
            _ => {
                let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(self.anchor, lat, lng);
                let d = wx.hypot(wz);
                if d > MAX_FLAT_RANGE_M {
                    return None;
                }
                let drop = crate::systems::globe::curvature_drop_m(d as f32, self.radius);
                Vec3::new(wx as f32, -drop, wz as f32)
            }
        };
        let up = if self.radius > 0.0 {
            (p - self.centre).normalize_or(Vec3::Y)
        } else {
            Vec3::Y
        };
        Some((p, up))
    }
}

/// One chunk of a board: its fills, and its edges where it has any.
pub struct Chunk {
    pub fill: Mesh,
    pub edges: Option<Mesh>,
    pub aabb: Aabb,
}

/// A colour texel's coordinates, as UV0 carries them.
fn texel(i: usize) -> [f32; 2] {
    [(i as u32 % ROW) as f32, (i as u32 / ROW) as f32]
}

/// Mesh a board for a frame of reference: its areas in chunks of nearby ones,
/// each a triangle list of fills and a line list of edges, every border once.
#[must_use]
pub fn build(set: &AreaSet, r: &Reference) -> Vec<Chunk> {
    let frame = (r.lay == Lay::Sphere).then(|| TangentFrame::new(r.globe_lat, r.globe_lng, 0.0));
    let edge_texel = texel(set.areas.len());
    let mut chunks = Vec::new();
    // A border two areas share is drawn by the first to reach it.
    let mut drawn: HashSet<[i64; 4]> = HashSet::new();
    let q = |v: f64| (v * 1e7).round() as i64;
    for areas in groups(set) {
        let (mut pos, mut up, mut uv, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut lpos, mut lup, mut luv) = (Vec::new(), Vec::new(), Vec::new());
        for a in areas {
            let area = &set.areas[a];
            let [w, s, e, n] = area.bbox;
            // Off the flat map's reach is off the board, whole.
            if frame.is_none() && r.place(None, (w + e) / 2.0, (s + n) / 2.0).is_none() {
                continue;
            }
            let (verts, tris) = area.triangles();
            let base = pos.len() as u32;
            let mut ok = true;
            for v in &verts {
                let Some((p, u)) = r.place(frame.as_ref(), v[0], v[1]) else {
                    ok = false;
                    break;
                };
                pos.push(p.to_array());
                up.push(u.to_array());
                uv.push(texel(a));
            }
            if !ok {
                pos.truncate(base as usize);
                up.truncate(base as usize);
                uv.truncate(base as usize);
                continue;
            }
            idx.extend(tris.iter().map(|t| base + t));
            for ring in area.polygons.iter().flatten() {
                for k in 0..ring.len() {
                    let (p0, p1) = (ring[k], ring[(k + 1) % ring.len()]);
                    let (a0, a1) = ([q(p0[0]), q(p0[1])], [q(p1[0]), q(p1[1])]);
                    if a0 == a1 {
                        continue;
                    }
                    let key = if a0 < a1 {
                        [a0[0], a0[1], a1[0], a1[1]]
                    } else {
                        [a1[0], a1[1], a0[0], a0[1]]
                    };
                    if !drawn.insert(key) {
                        continue;
                    }
                    for p in [p0, p1] {
                        if let Some((p, u)) = r.place(frame.as_ref(), p[0], p[1]) {
                            lpos.push(p.to_array());
                            lup.push(u.to_array());
                            luv.push(edge_texel);
                        }
                    }
                    // A segment one end of which would not place is dropped whole.
                    if lpos.len() % 2 == 1 {
                        lpos.pop();
                        lup.pop();
                        luv.pop();
                    }
                }
            }
        }
        if idx.is_empty() {
            continue;
        }
        let aabb = bounds(&pos);
        let mut fill = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        fill.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
        fill.insert_attribute(Mesh::ATTRIBUTE_NORMAL, up);
        fill.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
        fill.insert_indices(Indices::U32(idx));
        let edges = (!lpos.is_empty()).then(|| {
            let mut m = Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::RENDER_WORLD);
            m.insert_attribute(Mesh::ATTRIBUTE_POSITION, lpos);
            m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, lup);
            m.insert_attribute(Mesh::ATTRIBUTE_UV_0, luv);
            m
        });
        chunks.push(Chunk { fill, edges, aabb });
    }
    chunks
}

/// A board's areas in groups of neighbours: by the cell of a grid about
/// [`CHUNKS_ACROSS`] cells across the board that holds each area's middle.
fn groups(set: &AreaSet) -> Vec<Vec<usize>> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for a in &set.areas {
        b[0] = b[0].min(a.bbox[0]);
        b[1] = b[1].min(a.bbox[1]);
        b[2] = b[2].max(a.bbox[2]);
        b[3] = b[3].max(a.bbox[3]);
    }
    let cell = ((b[2] - b[0]).max(b[3] - b[1]) / CHUNKS_ACROSS).max(0.1);
    let mut out: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for (i, a) in set.areas.iter().enumerate() {
        let x = (((a.bbox[0] + a.bbox[2]) / 2.0 - b[0]) / cell).floor() as i64;
        let y = (((a.bbox[1] + a.bbox[3]) / 2.0 - b[1]) / cell).floor() as i64;
        out.entry((x, y)).or_default().push(i);
    }
    out.into_values().collect()
}

fn bounds(pos: &[[f32; 3]]) -> Aabb {
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for p in pos {
        lo = lo.min(Vec3::from(*p));
        hi = hi.max(Vec3::from(*p));
    }
    Aabb::from_min_max(lo, hi)
}

/// What the lens asks to be drawn this frame (`ScenarioView::lens_paint`).
pub struct LensPaint<'a> {
    /// The lens's file: another one starts the boards over.
    pub lens: &'a str,
    /// Every board of the lens, and the one shown now.
    pub sets: &'a [std::sync::Arc<AreaSet>],
    pub active: Option<usize>,
    /// Per area of the active board, the colour it is going to: sRGB, not
    /// premultiplied, 0–1, alpha the fill's opacity; transparent where it
    /// has no state.
    pub targets: Vec<[f32; 4]>,
    /// The edges' ink, the same way.
    pub edge: [f32; 4],
}

/// A finished worker job: the board it is for, the frame, and its chunks.
type Built = (usize, (Lay, u32, u64, u64, u32), Vec<Chunk>);

/// One board on the GPU.
struct Board {
    texture: Handle<Image>,
    material: Handle<LensAreaMaterial>,
    /// The drawn colours, eased towards the targets; same layout as them.
    colours: Vec<[f32; 4]>,
    /// Shown last frame: when it comes back its colours start from nothing,
    /// so a finer board arriving on a zoom fades in rather than popping.
    shown: bool,
    /// The frame its entities were built for, and them.
    built: Option<((Lay, u32, u64, u64, u32), Vec<Entity>)>,
    /// The frame a worker is building it for.
    pending: Option<(Lay, u32, u64, u64, u32)>,
    /// Chunks back from the worker, waiting for the frame's upload allowance.
    arrived: Vec<Chunk>,
    arrived_key: Option<(Lay, u32, u64, u64, u32)>,
}

/// The open lens's boards on the GPU.
#[derive(Resource, Default)]
pub struct LensAreas {
    lens: Option<String>,
    boards: Vec<Option<Board>>,
    inbox: Arc<Mutex<Vec<Built>>>,
}

/// Raised while a colour is still easing, for the egui side to keep the
/// frames coming (`scenario::render_scenario_map`).
static EASING: AtomicBool = AtomicBool::new(false);

/// Whether a lens colour is still on its way to its state's.
#[must_use]
pub fn easing() -> bool {
    EASING.load(Ordering::Relaxed)
}

impl LensAreas {
    fn clear(&mut self, commands: &mut Commands) {
        for b in self.boards.drain(..).flatten() {
            if let Some((_, es)) = b.built {
                for e in es {
                    commands.entity(e).try_despawn();
                }
            }
        }
        self.lens = None;
        // Jobs still on a worker land in an inbox nobody reads for this lens.
        self.inbox = Arc::default();
    }
}

/// Keep the open lens's boards meshed for the camera's frame, show the one
/// the scale picked, and ease its colours.
#[allow(clippy::too_many_arguments)]
pub fn draw_lens_areas(
    mut commands: Commands,
    mut gpu: ResMut<LensAreas>,
    view: Res<crate::systems::scenario::ScenarioView>,
    globe: Res<GlobeState>,
    map_anchor: Res<MapAnchor>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    root: Query<Entity, With<BigSpace>>,
    time: Res<Time>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<LensAreaMaterial>>,
) {
    let gpu = gpu.as_mut();
    let paint = view.lens_paint();
    let (Some(paint), Some(anchor), Ok((cell, tf))) = (paint, map_anchor.geo, camera.single())
    else {
        if gpu.lens.is_some() {
            gpu.clear(&mut commands);
        }
        EASING.store(false, Ordering::Relaxed);
        return;
    };
    if gpu.lens.as_deref() != Some(paint.lens) {
        gpu.clear(&mut commands);
        gpu.lens = Some(paint.lens.to_owned());
    }
    if gpu.boards.len() != paint.sets.len() {
        gpu.boards.resize_with(paint.sets.len(), || None);
    }
    let reference = Reference {
        lay: if globe.in_space(cell, tf) {
            Lay::Sphere
        } else {
            Lay::Flat
        },
        anchor,
        globe_lat: globe.anchor_lat,
        globe_lng: globe.anchor_lng,
        centre: globe.center_local,
        radius: if globe.ready { globe.radius } else { 0.0 },
        revision: globe.build_revision(),
    };
    let key = reference.key();

    // What the workers finished.
    let done: Vec<Built> = gpu
        .inbox
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    for (i, k, chunks) in done {
        if let Some(Some(b)) = gpu.boards.get_mut(i) {
            if b.pending == Some(k) {
                b.pending = None;
                b.arrived = chunks;
                b.arrived_key = Some(k);
            }
        }
    }

    let mut easing = false;
    let dt = time.delta_secs().min(0.1);
    let ease = 1.0 - (-EASE_PER_S * dt).exp();
    let root = root.single().ok();
    for (i, set) in paint.sets.iter().enumerate() {
        let active = paint.active == Some(i);
        let slot = &mut gpu.boards[i];
        if !active {
            if let Some(b) = slot.as_mut() {
                if b.shown {
                    b.shown = false;
                    if let Some((_, es)) = &b.built {
                        for &e in es {
                            commands.entity(e).try_insert(Visibility::Hidden);
                        }
                    }
                }
            }
            continue;
        }
        let b = slot.get_or_insert_with(|| {
            let n = set.areas.len() + 1;
            let rows = (n as u32).div_ceil(ROW).max(1);
            let mut image = Image::new_fill(
                Extent3d {
                    width: ROW,
                    height: rows,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                &[0, 0, 0, 0],
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
            );
            image.sampler = bevy::image::ImageSampler::nearest();
            let texture = images.add(image);
            let material = materials.add(LensAreaMaterial {
                params: Vec4::new(1.0, HORIZON_MARGIN_M, 0.0, 0.0),
                colours: texture.clone(),
            });
            Board {
                texture,
                material,
                colours: vec![[0.0; 4]; n],
                shown: false,
                built: None,
                pending: None,
                arrived: Vec::new(),
                arrived_key: None,
            }
        });

        // Meshed for this frame, or on its way.
        let built_now = b.built.as_ref().is_some_and(|(k, _)| *k == key);
        if !built_now && b.pending != Some(key) && b.arrived_key != Some(key) {
            b.pending = Some(key);
            let (set, inbox) = (set.clone(), gpu.inbox.clone());
            crate::platform::workers::submit_ranked(
                "lens areas",
                crate::platform::workers::NEAR,
                Box::new(move || {
                    let chunks = build(&set, &reference);
                    if let Ok(mut g) = inbox.lock() {
                        g.push((i, key, chunks));
                    }
                }),
            );
        }
        // Built for another frame: its positions are wrong in this one.
        if !built_now {
            if let Some((_, es)) = b.built.take() {
                for e in es {
                    commands.entity(e).try_despawn();
                }
            }
        }
        // Landing what came back, within the frame's upload allowance.
        if b.arrived_key == Some(key) {
            if let Some(root) = root {
                let entities = &mut b.built.get_or_insert_with(|| (key, Vec::new())).1;
                while let Some(chunk) = b.arrived.pop() {
                    let cost = crate::systems::stream_ledger::mesh_cost(&chunk.fill)
                        + chunk
                            .edges
                            .as_ref()
                            .map_or(0, crate::systems::stream_ledger::mesh_cost);
                    if !crate::systems::stream_ledger::admit(cost) {
                        b.arrived.push(chunk);
                        break;
                    }
                    let meshes_of = std::iter::once(chunk.fill).chain(chunk.edges);
                    for mesh in meshes_of {
                        entities.push(
                            commands
                                .spawn((
                                    Mesh3d(meshes.add(mesh)),
                                    MeshMaterial3d(b.material.clone()),
                                    chunk.aabb,
                                    Transform::default(),
                                    GridCell::default(),
                                    ChildOf(root),
                                    NotShadowCaster,
                                    NotShadowReceiver,
                                    Visibility::Visible,
                                ))
                                .id(),
                        );
                    }
                }
                if b.arrived.is_empty() {
                    b.arrived_key = None;
                }
            }
        }
        if !b.shown {
            b.shown = true;
            b.colours.iter_mut().for_each(|c| *c = [0.0; 4]);
            if let Some((_, es)) = &b.built {
                for &e in es {
                    commands.entity(e).try_insert(Visibility::Visible);
                }
            }
        }

        // The colours, eased; the texture written only when one moved.
        let mut moved = false;
        let n = b.colours.len();
        for (k, c) in b.colours.iter_mut().enumerate() {
            let target = if k + 1 == n {
                paint.edge
            } else {
                paint.targets.get(k).copied().unwrap_or([0.0; 4])
            };
            if *c == target {
                continue;
            }
            moved = true;
            if c.iter().zip(&target).all(|(x, y)| (x - y).abs() < 0.004) {
                *c = target;
            } else {
                easing = true;
                for j in 0..4 {
                    c[j] += (target[j] - c[j]) * ease;
                }
            }
        }
        if moved {
            let mut img = images.get_mut(&b.texture);
            if let Some(data) = img.as_mut().and_then(|img| img.data.as_mut()) {
                for (k, c) in b.colours.iter().enumerate() {
                    let px = &mut data[k * 4..k * 4 + 4];
                    for j in 0..4 {
                        px[j] = (c[j].clamp(0.0, 1.0) * 255.0).round() as u8;
                    }
                }
            }
        }
    }
    EASING.store(easing, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_scenario::board::Area;

    fn square(id: &str, x: f64, y: f64) -> Area {
        Area::new(
            id.into(),
            id.into(),
            "NL".into(),
            vec![vec![vec![
                [x, y],
                [x + 0.01, y],
                [x + 0.01, y + 0.01],
                [x, y + 0.01],
            ]]],
        )
    }

    fn flat() -> Reference {
        Reference {
            lay: Lay::Flat,
            anchor: (53.2, 6.56, 16),
            globe_lat: 53.2,
            globe_lng: 6.56,
            centre: Vec3::new(0.0, -6_364_000.0, 0.0),
            radius: 6_364_000.0,
            revision: 1,
        }
    }

    #[test]
    fn two_neighbours_share_their_border_once() {
        let set = AreaSet::new(
            "t".into(),
            String::new(),
            vec![square("a", 6.56, 53.2), square("b", 6.57, 53.2)],
        );
        let chunks = build(&set, &flat());
        let fills: usize = chunks.iter().map(|c| c.fill.count_vertices()).sum();
        let lines: usize = chunks
            .iter()
            .filter_map(|c| c.edges.as_ref())
            .map(Mesh::count_vertices)
            .sum();
        assert_eq!(fills, 8, "four corners an area");
        // Eight sides, one of them shared: seven segments, two ends each.
        assert_eq!(lines, 14);
    }

    #[test]
    fn an_area_past_the_flat_reach_is_left_off_but_kept_on_the_sphere() {
        let set = AreaSet::new("t".into(), String::new(), vec![square("far", 20.0, 53.2)]);
        assert!(build(&set, &flat()).is_empty());
        let sphere = Reference {
            lay: Lay::Sphere,
            ..flat()
        };
        assert_eq!(build(&set, &sphere).len(), 1);
    }

    #[test]
    fn every_vertex_names_its_own_areas_texel() {
        let areas = (0..300).map(|i| square(&i.to_string(), 6.0 + 0.011 * f64::from(i), 53.2));
        let set = AreaSet::new("t".into(), String::new(), areas.collect());
        let chunks = build(&set, &flat());
        let mut seen = HashSet::new();
        for c in &chunks {
            let Some(bevy::mesh::VertexAttributeValues::Float32x2(uv)) =
                c.fill.attribute(Mesh::ATTRIBUTE_UV_0)
            else {
                panic!("no uv");
            };
            for t in uv {
                seen.insert((t[0] as u32) + (t[1] as u32) * ROW);
            }
        }
        assert_eq!(seen.len(), 300);
        assert!(seen.contains(&299), "the 300th area is on the second row");
    }
}

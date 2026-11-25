//! The space behind the cosmos and its two star fields — the full-screen
//! fill with the globe's round hole, the Milky Way's ~7,000 points and the
//! galaxies' up to 50,000 — drawn as two Bevy 2D meshes on the library's own
//! camera (`lib_host::LibCamera`), under the library's cosmos
//! (`lib_cosmos`), which draws the bodies, rings and labels as boxes.
//!
//! **Meshes, not nodes.** A node per one-pixel square would be the heaviest
//! screen the library has. So `cosmos::render_cosmos` keeps the projection
//! and publishes the frame's points (`CosmosLib::lib_sky`, egui's points),
//! and this turns them into one mesh of small quads with vertex colours and
//! one of the space round the hole, both on a `ColorMaterial`. Quads rather
//! than `PrimitiveTopology::PointList`: wgpu has no point size (a point is
//! one pixel on every backend, WebGPU and the phone's Vulkan included), and
//! the dots are 1.5 to 4 points across.
//!
//! **Under the UI.** On a `Camera2d` the UI pass runs after the 2D passes
//! (`bevy_ui_render` puts it after `Core2dSystems::PostProcess`), so every
//! library node, the cosmos' boxes included, draws over these. The library's
//! camera draws over the world camera's picture, and so over egui as well:
//! while the cosmos shows, its space covers anything egui still draws.
//!
//! **Rebuilt only when it moved.** egui counts each change of the published
//! sky in `CosmosLib::sky_rev`; the meshes are rebuilt for a new count, or
//! when the camera's size or scale changes, and only as the frame's upload
//! ledger admits them (`stream_ledger::admit`): a refused rebuild keeps the
//! last meshes up and tries again next frame. Hidden below the ceiling.
//!
//! **Only where there is such a camera**: this sets `CosmosLib::sky` while
//! the library's camera exists, which is what tells egui to stop painting the
//! sky. As DOM (the web) there is none, and egui keeps painting it under the
//! DOM.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::sprite_render::{AlphaMode2d, ColorMaterial, MeshMaterial2d};
use egui::Color32;

use crate::systems::cosmos::{CosmosLib, CosmosSky};
use crate::systems::lib_host::LibCamera;
use crate::systems::lib_settings::LibScreens;

/// One of the two meshes.
#[derive(Component)]
pub struct CosmosSkyPart;

/// The meshes, their entities, and what they were last built for.
#[derive(Default)]
pub struct SkyState {
    /// The space round the hole, and the star fields' squares.
    fill: Option<(Entity, Handle<Mesh>)>,
    dots: Option<(Entity, Handle<Mesh>)>,
    /// The `sky_rev` and the camera (logical size, logical pixels per egui
    /// point) the meshes hold; `None` until built, and again once hidden.
    built: Option<(u64, Vec2, f32)>,
    /// Whether the last build had any squares.
    any_dots: bool,
}

/// Where the space mesh and the squares sit on the camera's z: the squares
/// over the space.
const FILL_Z: f32 = 0.0;
const DOTS_Z: f32 = 0.5;

/// Draws the cosmos' space and star fields on the library's camera while
/// egui publishes them, and hides them otherwise. `PostUpdate`, after
/// `cosmos::render_cosmos` (`Update`) has published the frame.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_cosmos_sky(
    lib: Res<LibScreens>,
    cosmos: Option<ResMut<CosmosLib>>,
    cameras: Query<&Camera, With<LibCamera>>,
    mut parts: Query<&mut Visibility, With<CosmosSkyPart>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut commands: Commands,
    mut state: Local<SkyState>,
) {
    if !lib.cosmos {
        return;
    }
    // Inserted by `lib_cosmos` on its first frame.
    let Some(mut cosmos) = cosmos else {
        return;
    };
    let camera = cameras.single().ok();
    if cosmos.sky != camera.is_some() {
        cosmos.sky = camera.is_some();
    }
    let target =
        camera.and_then(|c| Some((c.logical_viewport_size()?, c.target_scaling_factor()?)));
    let rev = cosmos.sky_rev;
    let (Some(sky), Some((size, scale_factor))) = (cosmos.lib_sky.as_ref(), target) else {
        state.built = None;
        show(&mut parts, &state, false);
        return;
    };
    if size.x <= 0.0 || size.y <= 0.0 || scale_factor <= 0.0 || sky.pixels_per_point <= 0.0 {
        return;
    }
    // Logical pixels of the camera's target per egui point.
    let s = sky.pixels_per_point / scale_factor;

    if state.fill.is_none() {
        let material = materials.add(ColorMaterial {
            color: Color::WHITE,
            alpha_mode: AlphaMode2d::Blend,
            ..Default::default()
        });
        let mut part = |z: f32| {
            let mesh = meshes.reserve_handle();
            let entity = commands
                .spawn((
                    Mesh2d(mesh.clone()),
                    MeshMaterial2d(material.clone()),
                    Transform::from_xyz(0.0, 0.0, z),
                    Visibility::Hidden,
                    // Never culled: the squares reach every corner, and a
                    // 2D mesh is in no light's cascades, so it casts shadows
                    // nowhere.
                    bevy::camera::visibility::NoFrustumCulling,
                    CosmosSkyPart,
                ))
                .id();
            (entity, mesh)
        };
        let fill = part(FILL_Z);
        let dots = part(DOTS_Z);
        state.fill = Some(fill);
        state.dots = Some(dots);
    }

    if state.built != Some((rev, size, s)) {
        let fill = fill_mesh(sky, size, s);
        let dots = (!sky.dots.is_empty()).then(|| dots_mesh(sky, size, s));
        let cost = crate::systems::stream_ledger::mesh_cost(&fill)
            + dots
                .as_ref()
                .map_or(0, crate::systems::stream_ledger::mesh_cost);
        if crate::systems::stream_ledger::admit(cost) {
            if let Some((_, handle)) = state.fill.as_ref() {
                // Reserved here: nothing else inserts into it.
                let _ = meshes.insert(handle, fill);
            }
            state.any_dots = dots.is_some();
            if let (Some((_, handle)), Some(dots)) = (state.dots.as_ref(), dots) {
                let _ = meshes.insert(handle, dots);
            }
            state.built = Some((rev, size, s));
        }
    }
    let built = state.built.is_some();
    show(&mut parts, &state, built);
}

/// Shows or hides the two meshes; the squares only when there are any.
fn show(parts: &mut Query<&mut Visibility, With<CosmosSkyPart>>, state: &SkyState, on: bool) {
    let mut set = |part: Option<&(Entity, Handle<Mesh>)>, on: bool| {
        let Some((entity, _)) = part else {
            return;
        };
        let want = if on {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if let Ok(mut v) = parts.get_mut(*entity) {
            if *v != want {
                *v = want;
            }
        }
    };
    set(state.fill.as_ref(), on);
    set(state.dots.as_ref(), on && state.any_dots);
}

/// An egui colour (premultiplied sRGB) as the linear, straight-alpha vertex
/// colour a `ColorMaterial` blends.
fn linear(c: Color32) -> [f32; 4] {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let lut = srgb_to_linear();
    [
        lut[usize::from(r)],
        lut[usize::from(g)],
        lut[usize::from(b)],
        f32::from(a) / 255.0,
    ]
}

/// sRGB's 256 levels in linear light, worked out once: up to 50,000 squares
/// a rebuild.
fn srgb_to_linear() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0; 256];
        for (i, v) in lut.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *v = if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
        }
        lut
    })
}

/// Places in the camera's world from the target's logical pixels (y down,
/// from the top left corner): the 2D camera looks at its middle, y up.
struct Frame {
    half: Vec2,
}

impl Frame {
    fn at(&self, x: f32, y: f32, z: f32) -> [f32; 3] {
        [x - self.half.x, self.half.y - y, z]
    }
}

fn mesh_of(positions: Vec<[f32; 3]>, colours: Vec<[f32; 4]>, indices: Vec<u32>) -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
    .with_inserted_indices(Indices::U32(indices))
}

/// Space over the whole target but the round hole the globe shows through:
/// egui's `fill_around` — the four bands round the hole's square, then the
/// ring between the square and the circle, its rays meeting the square's
/// corners exactly — in the camera's pixels.
fn fill_mesh(sky: &CosmosSky, size: Vec2, s: f32) -> Mesh {
    let frame = Frame { half: size / 2.0 };
    let ink = linear(sky.space);
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut quad = |pos: &mut Vec<[f32; 3]>, x0: f32, y0: f32, x1: f32, y1: f32| {
        let (x0, x1) = (x0.max(0.0), x1.min(size.x));
        let (y0, y1) = (y0.max(0.0), y1.min(size.y));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let i = pos.len() as u32;
        pos.extend([
            frame.at(x0, y0, 0.0),
            frame.at(x1, y0, 0.0),
            frame.at(x1, y1, 0.0),
            frame.at(x0, y1, 0.0),
        ]);
        idx.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
    };
    let hole = sky
        .hole
        .map(|(c, r)| (Vec2::new(c.x * s, c.y * s), r * s))
        .filter(|(c, r)| {
            c.is_finite()
                && r.is_finite()
                && *r > 0.0
                && c.x + r > 0.0
                && c.x - r < size.x
                && c.y + r > 0.0
                && c.y - r < size.y
        });
    let Some((c, r)) = hole else {
        quad(&mut pos, 0.0, 0.0, size.x, size.y);
        let colours = vec![ink; pos.len()];
        return mesh_of(pos, colours, idx);
    };
    let (min, max) = (c - Vec2::splat(r), c + Vec2::splat(r));
    // Above, below, left and right of the hole's square.
    quad(&mut pos, 0.0, 0.0, size.x, min.y);
    quad(&mut pos, 0.0, max.y, size.x, size.y);
    quad(&mut pos, 0.0, min.y, min.x, max.y);
    quad(&mut pos, max.x, min.y, size.x, max.y);
    // A multiple of eight, so a ray falls on each corner.
    let n = 64u32;
    let base = pos.len() as u32;
    for i in 0..=n {
        let a = std::f32::consts::TAU * i as f32 / n as f32;
        let d = Vec2::from_angle(a);
        let edge = d / d.x.abs().max(d.y.abs());
        let (on_circle, on_square) = (c + d * r, c + edge * r);
        pos.push(frame.at(on_circle.x, on_circle.y, 0.0));
        pos.push(frame.at(on_square.x, on_square.y, 0.0));
        if i > 0 {
            let k = base + 2 * i;
            idx.extend([k - 2, k - 1, k, k - 1, k + 1, k]);
        }
    }
    let colours = vec![ink; pos.len()];
    mesh_of(pos, colours, idx)
}

/// The star fields' squares, egui's paint order, in the camera's pixels.
fn dots_mesh(sky: &CosmosSky, size: Vec2, s: f32) -> Mesh {
    let frame = Frame { half: size / 2.0 };
    let n = sky.dots.len();
    let mut pos: Vec<[f32; 3]> = Vec::with_capacity(4 * n);
    let mut col: Vec<[f32; 4]> = Vec::with_capacity(4 * n);
    let mut idx: Vec<u32> = Vec::with_capacity(6 * n);
    for d in &sky.dots {
        let (x, y, r) = (d.at.x * s, d.at.y * s, d.r * s);
        if !(x.is_finite() && y.is_finite() && r.is_finite()) {
            continue;
        }
        let ink = linear(d.ink);
        let i = pos.len() as u32;
        pos.extend([
            frame.at(x - r, y - r, 0.0),
            frame.at(x + r, y - r, 0.0),
            frame.at(x + r, y + r, 0.0),
            frame.at(x - r, y + r, 0.0),
        ]);
        col.extend([ink; 4]);
        idx.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
    }
    mesh_of(pos, col, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sky(hole: Option<(egui::Pos2, f32)>) -> CosmosSky {
        CosmosSky {
            pixels_per_point: 1.0,
            space: Color32::from_rgb(4, 7, 10),
            hole,
            dots: vec![],
        }
    }

    #[test]
    fn without_a_hole_space_is_one_quad_over_the_whole_target() {
        let mesh = fill_mesh(&sky(None), Vec2::new(800.0, 600.0), 1.0);
        assert_eq!(mesh.count_vertices(), 4);
    }

    #[test]
    fn the_hole_s_ring_meets_the_circle_and_its_square() {
        let size = Vec2::new(800.0, 600.0);
        let mesh = fill_mesh(&sky(Some((egui::pos2(400.0, 300.0), 100.0))), size, 1.0);
        // Four bands and 65 rays of two.
        assert_eq!(mesh.count_vertices(), 16 + 130);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        // The first ray, to the right: the circle's point, then the square's,
        // both 100 px right of the middle, which is the camera's origin.
        assert!((p[16][0] - 100.0).abs() < 1e-3 && p[16][1].abs() < 1e-3);
        assert!((p[17][0] - 100.0).abs() < 1e-3 && p[17][1].abs() < 1e-3);
        // The ray at 45° reaches the square's corner, off the circle.
        assert!((p[16 + 16][0] - 100.0 / 2f32.sqrt()).abs() < 1e-3);
        assert!((p[17 + 16][0] - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_square_lands_where_egui_put_it() {
        let mut sky = sky(None);
        sky.pixels_per_point = 2.0;
        sky.dots.push(crate::systems::cosmos::SkyDot {
            at: egui::pos2(10.0, 20.0),
            r: 1.0,
            ink: Color32::WHITE,
        });
        // Twice the pixels per point at a scale factor of 1: 2 logical px.
        let mesh = dots_mesh(&sky, Vec2::new(100.0, 100.0), 2.0);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        // (20, 40) from the top left, ±2: from the middle, x −32…−28, y up 8…12.
        assert_eq!(p[0], [-32.0, 12.0, 0.0]);
        assert_eq!(p[2], [-28.0, 8.0, 0.0]);
    }
}

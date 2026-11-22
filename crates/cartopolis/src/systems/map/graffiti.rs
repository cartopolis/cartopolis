//! Which walls get graffiti: those along a narrow street.
//!
//! The paint itself is drawn by `building_windows.wesl` (`graffiti`), from the
//! wall's own coordinates; what it cannot see is what stands across the street
//! from it. This answers that once per wall face, on the worker that builds the
//! cell, and the answer rides in the facade code (`building_windows::Facade::graffiti`).
//!
//! A face counts when, straight out from it, a street of the tile's — a lane,
//! an alley, a footpath — runs before the next building's wall, and that wall is
//! no more than [`MAX_GAP_M`] away. The street is what tells a steeg from the
//! gap between two detached houses or the backs of two gardens, which are as
//! narrow and have no graffiti; a tile with no streets (a raster source) gives
//! no face any.
//!
//! Which stretches of a chosen wall carry a piece or a tag is the shader's hash,
//! not a placement anybody surveyed: the person asked for it (2026-11-22)
//! knowing it would not be the real walls' paint. The pieces themselves are
//! real Groningen graffiti, photographed for Commons ([`GraffitiSheet`]).

use bevy::math::Vec2;
use bevy::prelude::*;
use cartopolis_materials::building_windows::GRAFFITI_SHEET;
use std::collections::HashMap;

use crate::systems::building_windows::WallFace;

/// The narrowest gap counted: a party wall, or two parts of one building, sit
/// closer than any street.
pub const MIN_GAP_M: f32 = 1.2;
/// The widest: a steeg in Groningen is two to four metres wall to wall; past
/// six it is a street with pavements and windows on it.
pub const MAX_GAP_M: f32 = 6.5;
/// A face shorter than this is a jamb, a bay's side or a chimney breast.
const MIN_FACE_M: f32 = 2.5;
/// A face starting this high above its base is a setback storey, out of reach.
const MAX_FACE_LOW_M: f32 = 1.0;
/// The facing wall must run within this many degrees of parallel: a gable end
/// glimpsed at the end of a gap is not the other side of an alley.
const MAX_SKEW_DEG: f32 = 35.0;
/// Grid bucket edge for the segment index, metres.
const BUCKET_M: f32 = 4.0;

const STREET: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Seg {
    a: Vec2,
    b: Vec2,
    /// The building it outlines, or [`STREET`].
    owner: u32,
}

/// The cell's building outlines and street centre lines, bucketed for rays.
#[derive(Default)]
pub struct Alleys {
    segs: Vec<Seg>,
    buckets: HashMap<(i32, i32), Vec<u32>>,
}

fn bucket(p: Vec2) -> (i32, i32) {
    (
        (p.x / BUCKET_M).floor() as i32,
        (p.y / BUCKET_M).floor() as i32,
    )
}

impl Alleys {
    /// `outlines` one ring per building, indexed as [`Self::sprayed`]'s `own`;
    /// `streets` centre lines, both in the same world-XZ frame as the faces.
    #[must_use]
    pub fn new<'a>(
        outlines: impl IntoIterator<Item = &'a [Vec2]>,
        streets: impl IntoIterator<Item = Vec<Vec2>>,
    ) -> Self {
        let mut me = Self::default();
        for (i, ring) in outlines.into_iter().enumerate() {
            if ring.len() < 2 {
                continue;
            }
            for k in 0..ring.len() {
                me.push(ring[k], ring[(k + 1) % ring.len()], i as u32);
            }
        }
        for line in streets {
            for w in line.windows(2) {
                me.push(w[0], w[1], STREET);
            }
        }
        me
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    fn push(&mut self, a: Vec2, b: Vec2, owner: u32) {
        if a.distance_squared(b) < 1e-6 {
            return;
        }
        let id = self.segs.len() as u32;
        self.segs.push(Seg { a, b, owner });
        let (lo, hi) = (bucket(a.min(b)), bucket(a.max(b)));
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                self.buckets.entry((x, y)).or_default().push(id);
            }
        }
    }

    /// Whether `face`, a wall of building `own`, stands on a narrow street.
    #[must_use]
    pub fn sprayed(&self, own: u32, face: &WallFace) -> bool {
        if face.len < MIN_FACE_M || face.low > MAX_FACE_LOW_M || self.segs.is_empty() {
            return false;
        }
        let t = face.tangent();
        // Most of the face, not one point of it: a wall half on the alley and
        // half on the square it opens into is the square's.
        let hits = [0.2, 0.5, 0.8]
            .into_iter()
            .filter(|f| {
                let o = t * (face.u0 + f * face.len) + face.outward * (face.d + 0.05);
                self.across(own, o, face.outward, t)
            })
            .count();
        hits >= 2
    }

    /// From `o` along `dir`: is there a street, and then another building's
    /// wall running with `along` within the gap?
    fn across(&self, own: u32, o: Vec2, dir: Vec2, along: Vec2) -> bool {
        let end = o + dir * MAX_GAP_M;
        let (lo, hi) = (bucket(o.min(end)), bucket(o.max(end)));
        let parallel = MAX_SKEW_DEG.to_radians().cos();
        let mut wall = f32::INFINITY;
        let mut street = f32::INFINITY;
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                let Some(ids) = self.buckets.get(&(x, y)) else {
                    continue;
                };
                for &id in ids {
                    let s = self.segs[id as usize];
                    if s.owner == own {
                        continue;
                    }
                    let Some(at) = ray_hit(o, dir, s.a, s.b) else {
                        continue;
                    };
                    if s.owner == STREET {
                        street = street.min(at);
                    } else if (s.b - s.a).normalize().dot(along).abs() >= parallel {
                        wall = wall.min(at);
                    }
                }
            }
        }
        (MIN_GAP_M..=MAX_GAP_M).contains(&wall) && street < wall
    }
}

/// Distance along the unit ray `o + t·dir` to segment `a`–`b`, if it meets it.
fn ray_hit(o: Vec2, dir: Vec2, a: Vec2, b: Vec2) -> Option<f32> {
    let e = b - a;
    let denom = dir.perp_dot(e);
    if denom.abs() < 1e-6 {
        return None;
    }
    let ao = a - o;
    let t = ao.perp_dot(e) / denom;
    let s = ao.perp_dot(dir) / denom;
    (t > 0.0 && (0.0..=1.0).contains(&s)).then_some(t)
}

/// The graffiti sheet the shader paints from: real graffiti from Groningen, cut
/// out of Commons photographs by `tools/graffiti/gen.ts` (the credits are in
/// `assets/textures/graffiti.md`). Loaded once, given a mip chain on a worker —
/// a sheet seen from down an alley is minified hard — and block-compressed like
/// the map's tiles, then put in place of the
/// clear texel every building material has bound until then.
#[derive(Resource)]
pub struct GraffitiSheet {
    loading: Option<Handle<Image>>,
    built: std::sync::Arc<std::sync::Mutex<Option<Image>>>,
}

impl FromWorld for GraffitiSheet {
    fn from_world(world: &mut World) -> Self {
        use bevy::asset::RenderAssetUsages;
        use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
        let clear = Image::new_fill(
            Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 0],
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        world
            .resource_mut::<Assets<Image>>()
            .insert(&GRAFFITI_SHEET, clear)
            .expect("an asset id without a generation cannot be stale");
        let loading = world
            .resource::<AssetServer>()
            .load("textures/graffiti.png");
        Self {
            loading: Some(loading),
            built: Default::default(),
        }
    }
}

/// Once the sheet has decoded: mip it on a worker, then swap it in.
pub fn finish_graffiti_sheet(mut sheet: ResMut<GraffitiSheet>, mut images: ResMut<Assets<Image>>) {
    if let Some(image) = sheet.built.lock().ok().and_then(|mut b| b.take()) {
        if images.insert(&GRAFFITI_SHEET, image).is_err() {
            tracing::warn!("graffiti sheet: could not replace the placeholder");
        }
        return;
    }
    let Some(handle) = sheet.loading.as_ref() else {
        return;
    };
    // Whether the device decodes a block format is known once the renderer
    // has answered (`quality::capabilities`); until then wait, or the sheet
    // would go up uncompressed on a device that has one.
    if crate::quality::capabilities().is_none() {
        return;
    }
    let Some(decoded) = images.get(handle) else {
        return;
    };
    let size = decoded.size();
    let Some(data) = decoded.data.clone() else {
        sheet.loading = None;
        return;
    };
    sheet.loading = None;
    let out = sheet.built.clone();
    crate::platform::workers::submit(
        "graffiti",
        Box::new(move || {
            use bevy::image::{
                ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor,
            };
            // One upload, so bounded like any (`stream_ledger`, and the gate's
            // `upload_peak_asset_kb`): block-compressed as the map's tiles are,
            // 2048² and its mips are 5.6 MB; uncompressed they would be 22, so a
            // device with no block format gets the sheet at half the size.
            let (w, h, data) = if crate::quality::tile_compression().is_some() {
                (size.x, size.y, data)
            } else {
                (size.x / 2, size.y / 2, halve(size.x, size.y, &data))
            };
            let (texels, mips) = crate::systems::tile_loader::tile_texels(w, h, data);
            let image = crate::systems::tile_loader::image_from_mip_chain(
                w,
                h,
                texels.data,
                mips,
                texels.format,
                bevy::asset::RenderAssetUsages::RENDER_WORLD,
                ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::ClampToEdge,
                    address_mode_v: ImageAddressMode::ClampToEdge,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    mipmap_filter: ImageFilterMode::Linear,
                    anisotropy_clamp: 8,
                    ..Default::default()
                }),
            );
            if let Ok(mut b) = out.lock() {
                *b = Some(image);
            }
        }),
    );
}

/// An RGBA8 image at half its size, each texel the mean of four.
fn halve(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let (hw, hh) = ((w / 2) as usize, (h / 2) as usize);
    let w = w as usize;
    let mut out = vec![0u8; hw * hh * 4];
    for y in 0..hh {
        for x in 0..hw {
            for c in 0..4 {
                let at =
                    |dx: usize, dy: usize| u32::from(rgba[((2 * y + dy) * w + 2 * x + dx) * 4 + c]);
                out[(y * hw + x) * 4 + c] =
                    ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::building_windows::WallFaces;
    use bevy::math::Vec3;

    /// A box building's wall faces: `x0..x1` by `z0..z1`, 8 m tall, wound out.
    fn box_faces(x0: f32, x1: f32, z0: f32, z1: f32) -> WallFaces {
        let c = [
            Vec2::new(x0, z0),
            Vec2::new(x1, z0),
            Vec2::new(x1, z1),
            Vec2::new(x0, z1),
        ];
        let mut pts = Vec::new();
        let mut tris = Vec::new();
        for k in 0..4 {
            let (a, b) = (c[k], c[(k + 1) % 4]);
            let i = pts.len();
            pts.extend([
                Vec3::new(a.x, 0.0, a.y),
                Vec3::new(b.x, 0.0, b.y),
                Vec3::new(b.x, 8.0, b.y),
                Vec3::new(a.x, 8.0, a.y),
            ]);
            tris.push((i, i + 1, i + 2, 0));
            tris.push((i, i + 2, i + 3, 0));
        }
        let mut faces = WallFaces::new(&pts, &tris, 1);
        // Whichever way the winding came out, out is away from the middle.
        let mid = Vec2::new(x0 + x1, z0 + z1) * 0.5;
        for f in &mut faces.faces {
            if f.d - f.outward.dot(mid) < 0.0 {
                // The tangent turns with it, so the face's span does too.
                f.outward = -f.outward;
                f.d = -f.d;
                f.u0 = -(f.u0 + f.len);
            }
        }
        faces
    }

    fn ring(x0: f32, x1: f32, z0: f32, z1: f32) -> Vec<Vec2> {
        vec![
            Vec2::new(x0, z0),
            Vec2::new(x1, z0),
            Vec2::new(x1, z1),
            Vec2::new(x0, z1),
        ]
    }

    /// The faces of building 0 that face +x, across a gap of `gap` metres to
    /// building 1, with or without a footpath down the middle.
    fn east_face_sprayed(gap: f32, path: bool) -> bool {
        let a = ring(0.0, 10.0, 0.0, 20.0);
        let b = ring(10.0 + gap, 20.0 + gap, 0.0, 20.0);
        let x = 10.0 + gap * 0.5;
        let streets = path
            .then(|| vec![Vec2::new(x, -5.0), Vec2::new(x, 25.0)])
            .into_iter();
        let alleys = Alleys::new([a.as_slice(), b.as_slice()], streets);
        let faces = box_faces(0.0, 10.0, 0.0, 20.0);
        let east = faces
            .faces
            .iter()
            .find(|f| f.outward.x > 0.9)
            .expect("an east face");
        alleys.sprayed(0, east)
    }

    #[test]
    fn a_steeg_with_a_path_down_it_is_sprayed() {
        assert!(east_face_sprayed(3.0, true));
    }

    #[test]
    fn the_same_gap_with_no_street_in_it_is_not() {
        assert!(!east_face_sprayed(3.0, false));
    }

    #[test]
    fn a_party_wall_and_a_wide_street_are_not() {
        assert!(!east_face_sprayed(0.3, true));
        assert!(!east_face_sprayed(12.0, true));
    }

    #[test]
    fn a_wall_facing_open_ground_is_not() {
        let a = ring(0.0, 10.0, 0.0, 20.0);
        let alleys = Alleys::new(
            [a.as_slice()],
            [vec![Vec2::new(12.0, -5.0), Vec2::new(12.0, 25.0)]],
        );
        let faces = box_faces(0.0, 10.0, 0.0, 20.0);
        assert!(faces.faces.iter().all(|f| !alleys.sprayed(0, f)));
    }
}

//! The street crowd: the walkers `systems::traffic` moves, drawn as MakeHuman
//! people rather than boxes.
//!
//! The people are the avatar designer's starters (`avatar_designer::starter_looks`),
//! their skin families dealt out evenly and their skin tones spread, built from
//! the same pack the avatars are (`docs/notes/makehuman-avatars.md`) and cut
//! down by `cartopolis_human::crowd` to two levels of detail with their walk
//! baked into poses. Each body is one mesh per level and one material holding
//! its poses, shared by every walker wearing it; a walker's `MeshTag` carries
//! its step and its clothes' colours (`shaders/crowd_pose.wesl`), so the whole
//! crowd is a few instanced draws and uploads nothing once built.
//!
//! A body is made in four steps, each waiting on the last without holding the
//! frame: shaped on a worker, its textures loaded as small swatches, cut down
//! and baked on a worker, then uploaded — one body a frame, through the stream
//! ledger. A walker whose body is not up yet is not drawn.

use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::mesh::MeshTag;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use cartopolis_human::crowd::{CrowdBody, PartPaint, Region, Swatch};
use cartopolis_human::{HumanModel, PartKind, ProxyKind};
pub use cartopolis_materials::crowd::CrowdMaterial;
use cartopolis_materials::crowd::{CrowdExtension, CROWD_PALETTE};
use futures_channel::oneshot;

use crate::systems::human_avatar::{
    look_for, skin_colour, tint_of, HumanPackAsset, HumanPackHandle, Tints, PACK_DIR,
};

/// Triangles in each level of detail, nearest first. The full avatar is
/// ~33 000; at 3 000 a person still has a face and fingers' worth of hand
/// at a few metres, and at 600 the silhouette holds past [`NEAR_M`].
pub const LEVELS: [usize; 2] = [3000, 600];

/// Poses baked through one walk cycle; the shader blends between neighbours.
pub const WALK_POSES: usize = 16;

/// A walker nearer the camera than this wears the finer level.
pub const NEAR_M: f32 = 30.0;

/// How much further out a walker wearing the finer level keeps it, so one at
/// the threshold does not swap every frame.
const NEAR_HYSTERESIS_M: f32 = 4.0;

/// The longest side of a swatch, texels: a vertex samples it once, and the
/// finer level has ~2 000 vertices over a body.
const SWATCH_PX: u32 = 128;

/// The pose texture's width, texels; rows follow on.
const POSE_TEXTURE_WIDTH: u32 = 2048;

/// The skin families, dealt out to the starters in turn so the crowd holds
/// each about as often.
const FAMILIES: [&str; 3] = ["african", "asian", "caucasian"];

/// Muted outerwear. The same judgement the parked cars' palette makes: a crowd in
/// primary colours is the loudest thing on a pale cartographic street — but a
/// Dutch street in a raincoat season does carry the odd rust, bottle green and
/// trench beige among the greys.
pub const COAT_COLOURS: &[u32] = &[
    0x4A_4F_57, 0x6B_5E_52, 0x3E_4A_52, 0x7A_6E_63, 0x5A_4E_58, 0x86_7C_6E, 0x8C_4A_3A, 0x3F_5A_4A,
    0xB4_A4_86, 0x2B_2D_31,
];

/// Trousers: denim, black, charcoal, navy, khaki — what a street wears.
pub const TROUSER_COLOURS: &[u32] = &[
    0x3A_48_60, 0x26_27_29, 0x45_47_4B, 0x2C_34_46, 0x84_7A_64, 0x52_5A_6A,
];

/// A pack texture, small, decoded for the CPU to colour vertices from.
#[derive(Asset, TypePath)]
pub struct CrowdSwatch(pub Arc<Swatch>);

/// Reads a pack texture as a [`CrowdSwatch`]. It claims no extension: it is
/// found by the asset type asked for (`load::<CrowdSwatch>`), so the same
/// `*.avatar.jpg` files still go to `HumanTextureLoader` for the avatars.
#[derive(Default, TypePath)]
pub struct CrowdSwatchLoader;

impl AssetLoader for CrowdSwatchLoader {
    type Asset = CrowdSwatch;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let (done, swatch) = oneshot::channel();
        use crate::platform::workers;
        // Far: nothing on screen waits for the crowd.
        workers::submit_ranked(
            "crowd swatches",
            workers::FAR,
            Box::new(move || {
                let _ = done.send(swatch_from(&bytes));
            }),
        );
        swatch.await.map_err(std::io::Error::other)?
    }

    fn extensions(&self) -> &[&str] {
        &[]
    }
}

fn swatch_from(bytes: &[u8]) -> std::io::Result<CrowdSwatch> {
    let image = image::load_from_memory(bytes).map_err(std::io::Error::other)?;
    let image = if image.width().max(image.height()) > SWATCH_PX {
        image.resize(SWATCH_PX, SWATCH_PX, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.into_rgba8();
    Ok(CrowdSwatch(Arc::new(Swatch {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.pixels().map(|p| p.0).collect(),
    })))
}

/// One body on the GPU: per level, its mesh and its material.
pub struct CrowdBodyGpu {
    pub levels: Vec<(Handle<Mesh>, Handle<CrowdMaterial>)>,
    /// Metres walked in one walk cycle at scale 1.
    pub stride: f32,
    /// What every pose of every level fits in, for culling.
    pub bounds: Aabb,
}

/// Where one body is in being made.
enum Stage {
    Shaping(oneshot::Receiver<HumanModel>, Tints),
    Painting {
        model: Arc<HumanModel>,
        tints: Tints,
        swatches: Vec<Option<Handle<CrowdSwatch>>>,
    },
    Baking(oneshot::Receiver<CrowdBody>),
    Uploading(CrowdBody),
    Done,
}

/// The crowd's bodies, as far as they are made.
#[derive(Resource, Default)]
pub struct Crowd {
    /// One per starter; `None` until that body is uploaded.
    pub bodies: Vec<Option<CrowdBodyGpu>>,
    stages: Vec<Stage>,
    /// The pose library both crowd shaders import, held so it stays loaded
    /// (`docs/notes/shaders-are-wesl.md`).
    shader: Option<Handle<Shader>>,
}

impl Crowd {
    /// How many bodies the crowd has, made or not; walkers deal themselves
    /// out over this many.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bodies.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    #[must_use]
    pub fn body(&self, i: usize) -> Option<&CrowdBodyGpu> {
        self.bodies.get(i).and_then(Option::as_ref)
    }
}

/// Which body a walker wears and in what colours, and which level it is
/// drawn at. Set at spawn; the body is put on when it is made
/// ([`dress_walkers`]).
#[derive(Component, Clone, Copy, Debug)]
pub struct CrowdFigure {
    /// An index to deal over [`Crowd::len`] bodies.
    pub seed: u32,
    pub top: u8,
    pub bottom: u8,
    /// Wearing the finer level.
    pub near: bool,
}

impl CrowdFigure {
    #[must_use]
    pub fn body(&self, crowd: &Crowd) -> usize {
        self.seed as usize % crowd.len().max(1)
    }
}

/// The crowd's people: each starter, with the skin families dealt out in
/// turn and the tone spread a little either way.
fn crowd_looks() -> Vec<crate::systems::user_store::HumanLook> {
    crate::systems::avatar_designer::starter_looks()
        .into_iter()
        .enumerate()
        .map(|(i, mut look)| {
            look.skin = FAMILIES[i % FAMILIES.len()].into();
            look.skin_tone = ((i * 37) % 61) as i8 - 30;
            look
        })
        .collect()
}

/// Make the crowd's bodies, a step at a time, once the pack is in.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_crowd(
    mut crowd: ResMut<Crowd>,
    handle: Option<Res<HumanPackHandle>>,
    packs: Res<Assets<HumanPackAsset>>,
    asset_server: Res<AssetServer>,
    swatches: Res<Assets<CrowdSwatch>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<CrowdMaterial>>,
) {
    use crate::platform::workers;
    if crowd.stages.is_empty() {
        let Some(pack) = handle.as_ref().and_then(|h| h.get(&packs)) else {
            return;
        };
        crowd.shader = Some(asset_server.load("shaders/crowd_pose.wesl"));
        let looks = crowd_looks();
        crowd.bodies = looks.iter().map(|_| None).collect();
        crowd.stages = looks
            .iter()
            .map(|look| {
                let (look, tints) = look_for(look);
                let pack = pack.clone();
                let (done, receiver) = oneshot::channel();
                workers::submit_ranked(
                    "crowd body",
                    workers::FAR,
                    Box::new(move || {
                        let _ = done.send(cartopolis_human::build(&pack, &look));
                    }),
                );
                Stage::Shaping(receiver, tints)
            })
            .collect();
        return;
    }
    let mut uploaded = false;
    let Crowd { bodies, stages, .. } = &mut *crowd;
    for (i, stage) in stages.iter_mut().enumerate() {
        let next = match stage {
            Stage::Shaping(receiver, tints) => match receiver.try_recv() {
                Ok(Some(model)) => {
                    let swatches = model
                        .parts
                        .iter()
                        .map(|p| {
                            let t = p
                                .texture
                                .as_ref()
                                .filter(|_| cartopolis_human::crowd::drawn(p.kind))?;
                            Some(
                                asset_server
                                    .load::<CrowdSwatch>(format!("{PACK_DIR}/textures/{t}")),
                            )
                        })
                        .collect();
                    Some(Stage::Painting {
                        model: Arc::new(model),
                        tints: *tints,
                        swatches,
                    })
                }
                Ok(None) => None,
                Err(_) => {
                    warn!("crowd body {i}: the build was dropped; that body is left out");
                    Some(Stage::Done)
                }
            },
            Stage::Painting {
                model,
                tints,
                swatches: handles,
            } => {
                let settled = handles.iter().flatten().all(|h| {
                    swatches.contains(h)
                        || matches!(
                            asset_server.load_state(h),
                            bevy::asset::LoadState::Failed(_)
                        )
                });
                settled.then(|| {
                    let paints: Vec<(Option<Arc<Swatch>>, [f32; 3], Region)> = model
                        .parts
                        .iter()
                        .zip(handles.iter())
                        .map(|(part, h)| {
                            let swatch = h
                                .as_ref()
                                .and_then(|h| swatches.get(h))
                                .map(|s| s.0.clone());
                            let (tint, region) = paint_of(part, *tints);
                            (swatch, tint, region)
                        })
                        .collect();
                    let model = model.clone();
                    let (done, receiver) = oneshot::channel();
                    workers::submit_ranked(
                        "crowd bake",
                        workers::FAR,
                        Box::new(move || {
                            let paint: Vec<PartPaint> = paints
                                .iter()
                                .map(|(swatch, tint, region)| PartPaint {
                                    swatch: swatch.as_deref(),
                                    tint: *tint,
                                    region: *region,
                                })
                                .collect();
                            let _ = done.send(cartopolis_human::crowd::crowd_body(
                                &model, &paint, &LEVELS, WALK_POSES,
                            ));
                        }),
                    );
                    Stage::Baking(receiver)
                })
            }
            Stage::Baking(receiver) => match receiver.try_recv() {
                Ok(Some(body)) => Some(Stage::Uploading(body)),
                Ok(None) => None,
                Err(_) => {
                    warn!("crowd body {i}: the bake was dropped; that body is left out");
                    Some(Stage::Done)
                }
            },
            Stage::Uploading(body) => {
                let bytes: usize = body
                    .levels
                    .iter()
                    .map(|l| l.vertices() * (40 + 16 * (body.walk_poses + 1)) + l.indices.len() * 2)
                    .sum();
                (!uploaded && crate::systems::stream_ledger::admit(bytes)).then(|| {
                    uploaded = true;
                    bodies[i] = Some(upload(body, &mut meshes, &mut images, &mut materials));
                    info!(
                        "crowd body {i}: {:?} triangles, {} kB uploaded",
                        body.levels
                            .iter()
                            .map(|l| l.triangles())
                            .collect::<Vec<_>>(),
                        bytes / 1024
                    );
                    Stage::Done
                })
            }
            Stage::Done => None,
        };
        if let Some(next) = next {
            *stage = next;
        }
    }
}

/// The colour a part is painted in, linear, and its tint region: a garment
/// whose texture is a neutral grey takes the walker's own colour in the
/// shader, everything else is baked as the avatar would wear it.
fn paint_of(part: &cartopolis_human::Part, tints: Tints) -> ([f32; 3], Region) {
    let neutral = part.default_tint.is_some();
    let region = match part.kind {
        PartKind::Proxy(ProxyKind::Outfit) if neutral && part.region == 0 => Region::Top,
        PartKind::Proxy(ProxyKind::Outfit | ProxyKind::Bottom) if neutral => Region::Bottom,
        PartKind::Proxy(ProxyKind::Top) if neutral => Region::Top,
        _ => Region::Own,
    };
    let colour = match (region, part.kind) {
        (Region::Top | Region::Bottom, _) => Color::WHITE,
        (_, PartKind::Body) => skin_colour(tints.skin_tone),
        _ => tint_of(part.kind, part.region, part.default_tint, tints)
            .map_or(Color::WHITE, |c| Color::srgb_u8(c[0], c[1], c[2])),
    };
    let c = colour.to_linear();
    ([c.red, c.green, c.blue], region)
}

fn palette(colours: &[u32]) -> [Vec4; CROWD_PALETTE] {
    std::array::from_fn(|i| {
        let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(colours[i % colours.len()]);
        Vec4::new(r, g, b, 1.0)
    })
}

/// Put a baked body on the GPU: per level a mesh in its standing pose (the
/// shader poses it) and a material holding every pose.
fn upload(
    body: &CrowdBody,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<CrowdMaterial>,
) -> CrowdBodyGpu {
    let standing = body.walk_poses;
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let levels = body
        .levels
        .iter()
        .map(|level| {
            for pose in &level.positions {
                for p in pose {
                    lo = lo.min(Vec3::from_array(*p));
                    hi = hi.max(Vec3::from_array(*p));
                }
            }
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::RENDER_WORLD,
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, level.positions[standing].clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, level.normals[standing].clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, level.colours.clone());
            mesh.insert_indices(match u16::try_from(level.vertices()) {
                Ok(_) => Indices::U16(level.indices.iter().map(|&i| i as u16).collect()),
                Err(_) => Indices::U32(level.indices.clone()),
            });
            let texels = level.positions.len() * level.vertices() * 2;
            let height = texels.div_ceil(POSE_TEXTURE_WIDTH as usize) as u32;
            let mut data = Vec::with_capacity(POSE_TEXTURE_WIDTH as usize * height as usize * 8);
            for (positions, normals) in level.positions.iter().zip(&level.normals) {
                for (p, n) in positions.iter().zip(normals) {
                    for v in [p, n] {
                        for c in [v[0], v[1], v[2], 0.0] {
                            data.extend_from_slice(&f16_bits(c).to_le_bytes());
                        }
                    }
                }
            }
            data.resize(POSE_TEXTURE_WIDTH as usize * height as usize * 8, 0);
            let poses = images.add(Image::new(
                Extent3d {
                    width: POSE_TEXTURE_WIDTH,
                    height,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                data,
                TextureFormat::Rgba16Float,
                RenderAssetUsages::RENDER_WORLD,
            ));
            let material = materials.add(CrowdMaterial {
                base: StandardMaterial {
                    base_color: Color::WHITE,
                    perceptual_roughness: 0.8,
                    reflectance: 0.35,
                    ..default()
                },
                extension: CrowdExtension {
                    counts: UVec4::new(
                        level.vertices() as u32,
                        body.walk_poses as u32,
                        POSE_TEXTURE_WIDTH,
                        0,
                    ),
                    tops: palette(COAT_COLOURS),
                    bottoms: palette(TROUSER_COLOURS),
                    poses,
                },
            });
            (meshes.add(mesh), material)
        })
        .collect();
    CrowdBodyGpu {
        levels,
        stride: body.stride,
        bounds: Aabb::from_min_max(lo, hi),
    }
}

/// Put each walker's body on once it is made: the coarser level (the
/// traffic's drive picks the level from then on), its bounds over every pose,
/// and a tag for the shader.
pub fn dress_walkers(
    mut commands: Commands,
    crowd: Res<Crowd>,
    walkers: Query<(Entity, &CrowdFigure), Without<Mesh3d>>,
) {
    if crowd.is_empty() {
        return;
    }
    for (entity, figure) in &walkers {
        let Some(body) = crowd.body(figure.body(&crowd)) else {
            continue;
        };
        let (mesh, material) = body.levels.last().cloned().expect("a body has a level");
        commands.entity(entity).try_insert((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            MeshTag::new(tag(0.0, 1.0, figure)),
            body.bounds,
            NoAutoAabb,
        ));
    }
}

/// A walker's `MeshTag`, as `crowd_pose.wesl` reads it: `phase` through the
/// cycle (radians), how far it has slowed to a stand (0 walking … 1 still),
/// and its colours.
#[must_use]
pub fn tag(phase: f32, standing: f32, figure: &CrowdFigure) -> u32 {
    let cycle = (phase / std::f32::consts::TAU).rem_euclid(1.0);
    let phase = ((cycle * 4096.0) as u32).min(4095);
    let standing = (standing.clamp(0.0, 1.0) * 63.0).round() as u32;
    phase
        | standing << 12
        | (u32::from(figure.top) & 15) << 18
        | (u32::from(figure.bottom) & 15) << 22
}

/// The level of detail a walker at `distance` should wear.
#[must_use]
pub fn wants_near(figure: &CrowdFigure, distance: f32) -> bool {
    distance < NEAR_M + if figure.near { NEAR_HYSTERESIS_M } else { 0.0 }
}

/// An `f32` as IEEE half-precision bits, rounded to nearest, ties to even.
/// The values are positions within a couple of metres and unit normals, so
/// there is no NaN to keep.
#[must_use]
pub fn f16_bits(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let a = b & 0x7fff_ffff;
    if a >= 0x4780_0000 {
        // 65 536 and up: past the largest half.
        return sign | 0x7c00;
    }
    if a < 0x3880_0000 {
        // Below 2⁻¹⁴: a subnormal half, or zero.
        if a < 0x3300_0000 {
            return sign;
        }
        let e = a >> 23;
        let m = (a & 0x7f_ffff) | 0x80_0000;
        let shift = 126 - e;
        return sign | ((m + (1 << (shift - 1))) >> shift) as u16;
    }
    let r = a - 0x3800_0000;
    sign | ((r + 0x0fff + ((r >> 13) & 1)) >> 13) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_round_to_the_nearest() {
        assert_eq!(f16_bits(0.0), 0);
        assert_eq!(f16_bits(1.0), 0x3c00);
        assert_eq!(f16_bits(0.5), 0x3800);
        assert_eq!(f16_bits(-2.0), 0xc000);
        assert_eq!(f16_bits(1.0e9), 0x7c00);
        // 2⁻²⁰ is subnormal: 16 steps of 2⁻²⁴.
        assert_eq!(f16_bits(2.0_f32.powi(-20)), 0x0010);
        // 1 + 2⁻¹¹ lies halfway between 1 and the next half up: ties to even.
        assert_eq!(f16_bits(1.0 + 2.0_f32.powi(-11)), 0x3c00);
        assert_eq!(f16_bits(1.0 + 3.0 * 2.0_f32.powi(-11)), 0x3c02);
        // A position a metre and a half up keeps its millimetres.
        let back = |h: u16| {
            let e = i32::from((h >> 10) & 0x1f) - 15;
            (1.0 + f32::from(h & 0x3ff) / 1024.0) * 2.0_f32.powi(e)
        };
        assert!((back(f16_bits(1.537)) - 1.537).abs() < 0.001);
    }

    #[test]
    fn a_tag_packs_what_the_shader_unpacks() {
        let figure = CrowdFigure {
            seed: 3,
            top: 9,
            bottom: 5,
            near: false,
        };
        let t = tag(std::f32::consts::PI, 1.0, &figure);
        assert_eq!(t & 0xFFF, 2048, "half way through the cycle");
        assert_eq!((t >> 12) & 0x3F, 63, "stood still");
        assert_eq!((t >> 18) & 15, 9);
        assert_eq!((t >> 22) & 15, 5);
        assert_eq!(
            tag(-0.001, 0.0, &figure) & 0xFFF,
            4095,
            "a phase just under zero wraps"
        );
        assert_eq!(tag(std::f32::consts::TAU, 0.0, &figure) & 0xFFF, 0);
    }

    #[test]
    fn the_finer_level_is_kept_a_little_past_where_it_was_put_on() {
        let mut figure = CrowdFigure {
            seed: 0,
            top: 0,
            bottom: 0,
            near: false,
        };
        assert!(!wants_near(&figure, NEAR_M + 1.0));
        figure.near = true;
        assert!(wants_near(&figure, NEAR_M + 1.0));
        assert!(!wants_near(&figure, NEAR_M + NEAR_HYSTERESIS_M + 0.1));
    }

    #[test]
    fn the_crowd_deals_out_every_skin_family() {
        let looks = crowd_looks();
        assert!(looks.len() >= 12, "{} people", looks.len());
        for family in FAMILIES {
            let n = looks.iter().filter(|l| l.skin == family).count();
            assert!(
                n * FAMILIES.len() + FAMILIES.len() > looks.len(),
                "{family}: {n} of {}",
                looks.len()
            );
        }
        assert!(looks.iter().all(|l| (-30..=30).contains(&l.skin_tone)));
    }
}

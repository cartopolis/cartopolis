//! Every avatar is a person built at runtime from `cartopolis_human`'s pack: a
//! MakeHuman body shaped by its look, dressed, and animated with motion capture
//! (`docs/notes/makehuman-avatars.md`). The local avatar wears its own look,
//! every other one the look the server sent (protocol 42).
//!
//! The model is not a glTF scene: the joints, meshes and clips are made here
//! from a [`HumanModel`], and put under the avatar as one holder that
//! [`avatar`](super::avatar) drives — the `AvatarAnimations` table, the pose
//! and lean systems. What it has to
//! reproduce of `bevy_gltf` is the binding: each joint carries the
//! `AnimationTargetId` its name path hashes to, rooted at the holder's player,
//! and the clips are keyed by the same paths.

use std::sync::Arc;

use bevy::animation::animation_curves::AnimatableCurve;
use bevy::animation::{animated_field, AnimatedBy, AnimationTargetId};
use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy::curve::{Interval, SampleAutoCurve};
use bevy::ecs::system::SystemParam;
use bevy::mesh::morph::{MeshMorphWeights, MorphAttributes, MorphWeights};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use cartopolis_human::{HumanModel, HumanPack, Look, PartKind, Phenotype};

use crate::systems::avatar::ClipGait;
use crate::systems::user_store::{AvatarAppearance, HumanLook};

/// Where the pack and its textures live under `assets/`.
const PACK_DIR: &str = "models/avatar/human";

#[derive(Asset, TypePath)]
pub(crate) struct HumanPackAsset(pub Arc<HumanPack>);

#[derive(Default, TypePath)]
pub(crate) struct HumanPackLoader;

impl AssetLoader for HumanPackLoader {
    type Asset = HumanPackAsset;
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
        HumanPack::decode(&bytes)
            .map(|p| HumanPackAsset(Arc::new(p)))
            .map_err(std::io::Error::other)
    }

    fn extensions(&self) -> &[&str] {
        &["humanpack"]
    }
}

/// The pack's textures (`*.avatar.jpg`, `*.avatar.png` — the inner extension
/// is what routes them here rather than to Bevy's image loader), mipmapped and
/// block-compressed for this GPU the way map tiles are
/// (`tile_loader::tile_texels`).
///
/// Measured 2026-10-18 (the `debug!` below, on mac-1, BC3): the default man
/// draws seven textures — three 1024², three 512², one 256² — which Bevy's
/// image loader put on the GPU as 16.0 MB of RGBA8 with no mips; compressed,
/// mips included, they are 5.3 MB. ETC2 blocks are the same size. Encoding
/// them took 352 ms of worker time in all. The mips matter on their own too:
/// an avatar is mostly seen from a follow camera's distance, where an
/// unmipped 1024² skin shimmers.
///
/// The decode and the encode run on `platform::workers`, never the asset
/// loader's own thread: a 1024² ETC2 chain is tens of milliseconds of blocking
/// CPU, and Bevy's IO pool is where every other asset waits.
#[derive(Default, TypePath)]
pub(crate) struct HumanTextureLoader;

impl AssetLoader for HumanTextureLoader {
    type Asset = Image;
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
        let (done, image) = futures_channel::oneshot::channel();
        use crate::platform::workers;
        // Near: it is the body on screen, and nothing draws it until all its
        // textures are in.
        workers::submit_ranked(
            "avatar textures",
            workers::NEAR,
            Box::new(move || {
                let _ = done.send(human_texture(&bytes));
            }),
        );
        image.await.map_err(std::io::Error::other)?
    }

    fn extensions(&self) -> &[&str] {
        &["avatar.jpg", "avatar.png"]
    }
}

/// Decode a pack texture and make it the image the GPU gets: its whole mip
/// chain, block-compressed where this device decodes a block format.
fn human_texture(bytes: &[u8]) -> std::io::Result<Image> {
    use crate::systems::tile_loader::{image_from_mip_chain, tile_texels};
    let rgba = image::load_from_memory(bytes)
        .map_err(std::io::Error::other)?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let (texels, mip_count) = tile_texels(width, height, rgba.into_raw());
    debug!(
        "avatar texture {width}x{height}: {:?}, {} mips, {} bytes (RGBA8 unmipped: {})",
        texels.format,
        mip_count,
        texels.data.len(),
        width * height * 4
    );
    Ok(image_from_mip_chain(
        width,
        height,
        texels.data,
        mip_count,
        texels.format,
        RenderAssetUsages::RENDER_WORLD,
        bevy::image::ImageSampler::linear(),
    ))
}

#[derive(Resource)]
pub(crate) struct HumanPackHandle(Handle<HumanPackAsset>);

/// Start loading the pack.
pub(crate) fn load_human_pack(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(HumanPackHandle(
        asset_server.load(format!("{PACK_DIR}/default.humanpack")),
    ));
}

/// The pack's clips the avatar's table is built from, in its order: idle, walk,
/// run, the six emotes in `EMOTE_CLIPS` order, and sitting. One the pack does
/// not carry plays the idle instead — an empty clip would drop the body into
/// its A-shaped rest pose.
pub(crate) const CLIP_ORDER: [&str; 10] = [
    "idle", "walk", "run", "wave", "interact", "roll", "punch", "kick", "faint", "sit",
];

/// What the spawn needs to build a person.
#[derive(SystemParam)]
pub(crate) struct HumanAssets<'w> {
    handle: Option<Res<'w, HumanPackHandle>>,
    draft: Res<'w, HumanDraft>,
    remote_looks: Res<'w, RemoteLooks>,
    packs: Res<'w, Assets<HumanPackAsset>>,
    asset_server: Res<'w, AssetServer>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
    textures: ResMut<'w, HumanTextures>,
}

/// Every pack texture the designer has shown this time it is open, held so
/// that going back to a skin or a hair tried a moment ago is not another load
/// and encode (352 ms of worker time for the default man). Emptied when the
/// designer closes; what the avatars wear stays loaded through their own
/// materials.
#[derive(Resource, Default)]
pub(crate) struct HumanTextures(std::collections::HashMap<String, Handle<Image>>);

impl HumanAssets<'_> {
    /// A pack texture by its file name, kept while the designer is open.
    fn texture(&mut self, name: &str) -> Handle<Image> {
        if let Some(h) = self.textures.0.get(name) {
            return h.clone();
        }
        let h = self
            .asset_server
            .load(format!("{PACK_DIR}/textures/{name}"));
        if self.draft.0.is_some() {
            self.textures.0.insert(name.to_string(), h.clone());
        }
        h
    }

    /// The look the local avatar should be built in now ([`current_look`]).
    pub(crate) fn look(&self, appearance: &AvatarAppearance) -> (Look, Tints) {
        look_for(&current_look(appearance, &self.draft))
    }

    /// The look another avatar should be built in ([`RemoteLooks::look_of`]).
    pub(crate) fn look_of_remote(&self, sim_id: u64) -> (Look, Tints) {
        look_for(&self.remote_looks.look_of(sim_id))
    }

    /// The pack, once it has loaded.
    pub(crate) fn pack(&self) -> Option<Arc<HumanPack>> {
        let h = self.handle.as_ref()?;
        self.packs.get(&h.0).map(|p| p.0.clone())
    }

    /// The pack will never load (missing, an LFS pointer, an old layout): the
    /// spawn says so once rather than waiting for it all session.
    pub(crate) fn failed(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| {
            matches!(
                self.asset_server.load_state(&h.0),
                bevy::asset::LoadState::Failed(_)
            )
        })
    }
}

/// What the designer is showing, while it is open: the avatar is built from
/// this draft rather than the saved look, so a slider moves the body as it is
/// dragged. `None` when the designer is closed.
#[derive(Resource, Default)]
pub(crate) struct HumanDraft(pub Option<HumanLook>);

/// The look the local avatar should wear now: the designer's draft if one is
/// open, else the saved one, else the default for the saved body.
pub(crate) fn current_look(appearance: &AvatarAppearance, draft: &HumanDraft) -> HumanLook {
    draft
        .0
        .clone()
        .or_else(|| appearance.human.clone())
        .unwrap_or_else(|| {
            with_overrides(HumanLook::default_for(
                appearance.sex == crate::systems::user_store::AvatarSex::Woman,
            ))
        })
}

/// `CARTO_HUMAN_LOOK="body_shape=0,age=70,hair=afro01,top=150.40.40,face:nose/nose-scale-horiz-decr-incr=80"`:
/// start an undesigned avatar with these, so a capture — which cannot drag a
/// slider — can photograph a look other than the default. Colours are
/// `r.g.b`; unknown keys and bad values are ignored.
fn with_overrides(mut look: HumanLook) -> HumanLook {
    let Some(spec) = crate::debug_switches::env_value("CARTO_HUMAN_LOOK") else {
        return look;
    };
    // Body shape first, so a feminine body's default wardrobe can be set
    // before anything the spec names overrides it.
    let pairs: Vec<(&str, &str)> = spec
        .split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    if let Some(v) = pairs
        .iter()
        .find(|(k, _)| *k == "body_shape")
        .and_then(|(_, v)| v.parse::<u8>().ok())
    {
        look.body_shape = v.min(100);
        if v < 50 {
            look.outfit = "female_elegantsuit01".into();
            look.hair = Some("bob01".into());
            look.shoes = "shoes01".into();
        }
    }
    let colour = |v: &str| -> Option<[u8; 3]> {
        let c: Vec<u8> = v.split('.').filter_map(|x| x.parse().ok()).collect();
        (c.len() == 3).then(|| [c[0], c[1], c[2]])
    };
    for (k, v) in pairs {
        let n = v.parse::<i32>().ok();
        match (k, n) {
            ("age", Some(n)) => look.age = n.clamp(18, 90) as u8,
            ("height", Some(n)) => look.height = n.clamp(0, 100) as u8,
            ("weight", Some(n)) => look.weight = n.clamp(0, 100) as u8,
            ("muscle", Some(n)) => look.muscle = n.clamp(0, 100) as u8,
            ("proportions", Some(n)) => look.proportions = n.clamp(0, 100) as u8,
            ("chest", Some(n)) => look.chest = n.clamp(0, 100) as u8,
            ("skin", _) => look.skin = v.into(),
            ("eyes", _) => look.eyes = v.into(),
            ("hair", _) => look.hair = (v != "none").then(|| v.into()),
            ("eyebrows", _) => look.eyebrows = v.into(),
            ("outfit", _) => look.outfit = v.into(),
            ("shoes", _) => look.shoes = v.into(),
            ("hat", _) => look.hat = (v != "none").then(|| v.into()),
            ("hair_colour", _) => look.hair_colour = colour(v),
            ("top", _) => look.top_colour = colour(v),
            ("bottom", _) => look.bottom_colour = colour(v),
            (face, Some(n)) if face.starts_with("face:") => {
                look.set_face(&face[5..], n.clamp(-100, 100) as i8);
            }
            _ => {}
        }
    }
    look
}

/// The colours a look puts over the neutral textures; `None` keeps the
/// garment's or the hair's own (its region's recorded colour).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Tints {
    pub hair: Option<[u8; 3]>,
    pub top: Option<[u8; 3]>,
    pub bottom: Option<[u8; 3]>,
}

/// MakeHuman's age slider from years: 0.1875 is 11, 0.5 is 25 and 1.0 is 90,
/// linear between (`macro.json`'s bands and MakeHuman's own labels).
fn age_value(years: u8) -> f32 {
    let y = f32::from(years.clamp(1, 90));
    if y <= 25.0 {
        0.1875 + (y - 11.0) / 14.0 * 0.3125
    } else {
        0.5 + (y - 25.0) / 65.0 * 0.5
    }
}

/// The skin for a family at an age and body shape: MakeHuman painted each
/// family young, middle-aged and old, female and male.
fn skin_name(family: &str, years: u8, body_shape: u8) -> String {
    let age = match years {
        0..=35 => "young",
        36..=59 => "middleage",
        _ => "old",
    };
    let sex = if body_shape < 50 { "female" } else { "male" };
    format!("{age}_{family}_{sex}")
}

/// A designed look as the body builder and the materials take it.
pub(crate) fn look_for(h: &HumanLook) -> (Look, Tints) {
    let pct = |v: u8| f32::from(v.min(100)) / 100.0;
    let phenotype = Phenotype {
        gender: pct(h.body_shape),
        age: age_value(h.age),
        muscle: pct(h.muscle),
        weight: pct(h.weight),
        height: pct(h.height),
        proportions: pct(h.proportions),
        cupsize: pct(h.chest),
        ..Phenotype::default()
    };
    let mut proxies = vec![
        "high-poly".to_string(),
        "eyelashes01".into(),
        h.eyebrows.clone(),
    ];
    proxies.extend(h.hair.clone());
    proxies.push(h.outfit.clone());
    proxies.push(h.shoes.clone());
    proxies.extend(h.hat.clone());
    (
        Look {
            phenotype,
            skin: skin_name(&h.skin, h.age, h.body_shape),
            proxies,
            eyes: Some(h.eyes.clone()),
            modifiers: h
                .face
                .iter()
                .map(|(m, v)| (m.clone(), f32::from(*v) / 100.0))
                .collect(),
        },
        Tints {
            hair: h.hair_colour,
            top: h.top_colour,
            bottom: h.bottom_colour,
        },
    )
}

/// A spawned person: the holder entity (to be given the player and the
/// animation table) and the clips in [`CLIP_ORDER`].
pub(crate) struct SpawnedHuman {
    pub holder: Entity,
    pub clips: Vec<Handle<AnimationClip>>,
    pub walk: ClipGait,
    pub run: ClipGait,
    /// Each emote's length, in `CLIP_ORDER`'s emote order.
    pub emote_secs: Vec<f32>,
}

/// A person built and turned into engine data — meshes, clips, the joints'
/// rest transforms — with nothing yet in the world. Everything here is plain
/// data, so it can be made on a worker ([`prepare_human`]); only
/// [`spawn_human`]'s asset handles and entities need the main thread.
pub(crate) struct PreparedHuman {
    joints: Vec<PreparedJoint>,
    inverse_binds: Vec<Mat4>,
    parts: Vec<(Mesh, HumanPart, Name)>,
    /// One per [`CLIP_ORDER`] name; `None` plays the idle, which is first.
    clips: Vec<Option<AnimationClip>>,
    walk: ClipGait,
    run: ClipGait,
    emote_secs: Vec<f32>,
    /// Each hand round a phone, and every joint at rest in the holder's
    /// frame: what `held_phone` poses an arm from.
    phone_grip: [Option<cartopolis_human::hands::PhoneGrip>; 2],
    rest: Vec<Vec3>,
    height: f32,
    vertices: usize,
    built_ms: f32,
    missing_targets: usize,
}

struct PreparedJoint {
    name: Name,
    parent: Option<usize>,
    transform: Transform,
    target: AnimationTargetId,
}

/// What a drawn part of a person is, kept on its mesh entity so a colour
/// change can repaint it without building the person again
/// ([`rebuild_on_look_change`]).
#[derive(Component, Clone, Debug)]
pub(crate) struct HumanPart {
    kind: PartKind,
    region: u8,
    texture: Option<String>,
    alpha: bool,
    default_tint: Option<[u8; 3]>,
    /// The mesh carries the face's units as morph targets, weighted by the
    /// holder's `MorphWeights` (`systems::avatar_face`).
    morphs: bool,
}

/// Build `look` from `pack` into [`PreparedHuman`]: the shaping, the fitting
/// and the retargeting, then the meshes and clips made of them. Pure CPU work
/// with no world access — a few milliseconds natively, more on a phone — so a
/// rebuild runs it on `platform::workers` and the frame never waits for it.
pub(crate) fn prepare_human(pack: &HumanPack, look: &Look) -> PreparedHuman {
    let started = bevy::platform::time::Instant::now();
    let model = cartopolis_human::build(pack, look);

    // Each joint's animation target is its name path from the holder, the
    // binding `bevy_gltf` would have made.
    let holder_name = Name::new(HOLDER_NAME);
    let mut paths: Vec<Vec<Name>> = Vec::with_capacity(model.joints.len());
    let joints = model
        .joints
        .iter()
        .map(|j| {
            let mut path = match j.parent {
                Some(p) => paths[p].clone(),
                None => vec![holder_name.clone()],
            };
            path.push(Name::new(j.name.clone()));
            let target = AnimationTargetId::from_names(path.iter());
            paths.push(path);
            PreparedJoint {
                name: Name::new(j.name.clone()),
                parent: j.parent,
                transform: Transform {
                    translation: Vec3::from_array(j.translation),
                    rotation: Quat::from_array(j.rotation),
                    scale: Vec3::ONE,
                },
                target,
            }
        })
        .collect();
    let inverse_binds = model
        .joints
        .iter()
        .map(|j| Mat4::from_cols_array(&j.inverse_bind))
        .collect();

    let mut vertices = 0;
    let parts = model
        .parts
        .iter()
        .map(|part| {
            vertices += part.positions.len();
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.positions.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, part.normals.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, part.uvs.clone());
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_JOINT_INDEX,
                VertexAttributeValues::Uint16x4(part.joints.clone()),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, part.weights.clone());
            mesh.insert_indices(Indices::U32(part.indices.clone()));
            if !part.morphs.is_empty() {
                // Target after target, every vertex of each: the engine's
                // layout. Tangents are not animated (the skin has none).
                mesh.set_morph_targets(
                    part.morphs
                        .iter()
                        .flat_map(|t| {
                            t.positions.iter().zip(&t.normals).map(|(p, n)| {
                                MorphAttributes::new(
                                    Vec3::from_array(*p),
                                    Vec3::from_array(*n),
                                    Vec3::ZERO,
                                )
                            })
                        })
                        .collect(),
                );
                mesh.set_morph_target_names(
                    cartopolis_human::face::UNITS.map(String::from).to_vec(),
                );
            }
            let info = HumanPart {
                kind: part.kind,
                region: part.region,
                texture: part.texture.clone(),
                alpha: part.alpha,
                default_tint: part.default_tint,
                morphs: !part.morphs.is_empty(),
            };
            (mesh, info, Name::new(part.name.clone()))
        })
        .collect();

    let find = |name: &str| model.clips.iter().find(|c| c.name == name);
    let clips = CLIP_ORDER
        .iter()
        .map(|name| match find(name) {
            Some(c) => Some(animation_clip(c, &model, &paths)),
            // An idle the pack lacks is still a clip, an empty one.
            None if *name == "idle" => Some(AnimationClip::default()),
            None => None,
        })
        .collect();
    // A captured cycle's own length and speed; a pack without the clip keeps
    // the characters' numbers, which only time a clip that is not there.
    let gait = |name: &str, fallback: ClipGait| {
        find(name)
            .filter(|c| c.speed > 0.0)
            .map_or(fallback, |c| ClipGait {
                secs: c.duration(),
                speed: c.speed,
            })
    };
    // A gesture the pack lacks plays the idle, for as long as the characters'
    // emotes last.
    let emote_secs = CLIP_ORDER[3..9]
        .iter()
        .map(|n| find(n).map_or(1.6, |c| c.duration()))
        .collect();
    PreparedHuman {
        joints,
        inverse_binds,
        parts,
        clips,
        walk: gait("walk", ClipGait::CHARACTER_WALK),
        run: gait("run", ClipGait::CHARACTER_RUN),
        emote_secs,
        phone_grip: model.phone_grip.clone(),
        rest: model.joint_positions(None),
        height: model.height,
        vertices,
        built_ms: started.elapsed().as_secs_f32() * 1000.0,
        missing_targets: model.missing_targets.len(),
    }
}

/// The holder's name, the root of every joint's animation target path.
const HOLDER_NAME: &str = "human";

/// Put a prepared person under `parent` as one holder. `holder` bundles what
/// the caller's avatar code tags a holder with. With `hidden`, every mesh is
/// spawned hidden, for a rebuild to show once its textures are in
/// ([`finish_rebuilds`]) — the holder's own visibility stays the first-person
/// and driver systems' to write.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_human(
    commands: &mut Commands,
    assets: &mut HumanAssets,
    clip_assets: &mut Assets<AnimationClip>,
    prepared: PreparedHuman,
    tints: Tints,
    parent: Entity,
    holder: impl Bundle,
    hidden: bool,
) -> SpawnedHuman {
    let holder = commands
        .spawn((
            holder,
            Name::new(HOLDER_NAME),
            Transform::default(),
            Visibility::default(),
            ChildOf(parent),
        ))
        .id();

    // Joints, parents first (the model's order), each bound to the holder's
    // player.
    let mut joints: Vec<Entity> = Vec::with_capacity(prepared.joints.len());
    let names: Vec<String> = prepared.joints.iter().map(|j| j.name.to_string()).collect();
    for j in prepared.joints {
        let entity = commands
            .spawn((
                j.name,
                j.transform,
                Visibility::default(),
                j.target,
                AnimatedBy(holder),
                ChildOf(j.parent.map_or(holder, |p| joints[p])),
            ))
            .id();
        joints.push(entity);
    }
    let bindposes = assets
        .bindposes
        .add(SkinnedMeshInverseBindposes::from(prepared.inverse_binds));

    // The arms a phone can be held in (`held_phone`), and the phone, hidden
    // until one is.
    {
        use super::held_phone::{ArmJoints, HeldPhoneMesh, HumanArms, Side, PHONE_SIZE};
        let index = |name: &str| names.iter().position(|n| n == name);
        let joint = |name: &str| index(name).map(|i| (joints[i], prepared.rest[i]));
        let entity = |i: usize| joints.get(i).copied();
        let arm = |side: Side, grip: &Option<cartopolis_human::hands::PhoneGrip>| {
            grip.as_ref()
                .and_then(|g| ArmJoints::new(side, g, joint, entity))
        };
        let phone = commands
            .spawn((
                Name::new("held phone"),
                HeldPhoneMesh,
                Mesh3d(assets.meshes.add(Cuboid::from_size(PHONE_SIZE))),
                MeshMaterial3d(assets.materials.add(StandardMaterial {
                    base_color: Color::srgb(0.04, 0.04, 0.05),
                    perceptual_roughness: 0.3,
                    ..default()
                })),
                Transform::default(),
                Visibility::Hidden,
                ChildOf(holder),
            ))
            .id();
        let [left, right] = &prepared.phone_grip;
        commands.entity(holder).insert(HumanArms {
            arms: [arm(Side::Left, left), arm(Side::Right, right)],
            head: index("head").map_or(Vec3::Y * 1.6, |i| prepared.rest[i]),
            phone,
        });
    }

    // The face's weights, on the holder, for every part that has its units.
    if prepared.parts.iter().any(|(_, info, _)| info.morphs) {
        if let Ok(weights) = MorphWeights::new(vec![0.0; cartopolis_human::face::UNITS.len()], None)
        {
            commands.entity(holder).insert(weights);
        }
    }
    let parts = prepared.parts.len();
    for (mesh, info, name) in prepared.parts {
        let material = material_for(&info, assets, tints);
        let morphs = info.morphs;
        let mut part = commands.spawn((
            name,
            Mesh3d(assets.meshes.add(mesh)),
            MeshMaterial3d(assets.materials.add(material)),
            SkinnedMesh {
                inverse_bindposes: bindposes.clone(),
                joints: joints.clone(),
            },
            info,
            Transform::default(),
            if hidden {
                Visibility::Hidden
            } else {
                Visibility::default()
            },
            ChildOf(holder),
        ));
        if morphs {
            part.insert(MeshMorphWeights::Reference(holder));
        }
    }

    let mut clips_in = prepared.clips.into_iter();
    let idle = clip_assets.add(clips_in.next().flatten().unwrap_or_default());
    let clips = std::iter::once(idle.clone())
        .chain(clips_in.map(|c| c.map_or_else(|| idle.clone(), |c| clip_assets.add(c))))
        .collect();

    tracing::info!(
        height_m = prepared.height,
        joints = joints.len(),
        parts,
        vertices = prepared.vertices,
        built_ms = prepared.built_ms,
        missing_targets = prepared.missing_targets,
        "avatar: MakeHuman body built"
    );
    SpawnedHuman {
        holder,
        clips,
        walk: prepared.walk,
        run: prepared.run,
        emote_secs: prepared.emote_secs,
    }
}

/// The tint a part takes under `tints`: the look's for this part if it names
/// one, else the colour the pack recorded for it; `None` for an authored
/// texture, which takes none.
fn tint_for(part: &HumanPart, tints: Tints) -> Option<[u8; 3]> {
    use cartopolis_human::ProxyKind;
    let chosen = match part.kind {
        PartKind::Proxy(ProxyKind::Hair | ProxyKind::Eyebrows | ProxyKind::Eyelashes) => tints.hair,
        PartKind::Proxy(ProxyKind::Outfit) if part.region == 0 => tints.top,
        PartKind::Proxy(ProxyKind::Outfit) => tints.bottom,
        _ => None,
    };
    chosen.or(part.default_tint)
}

fn base_colour(part: &HumanPart, tints: Tints) -> Color {
    tint_for(part, tints).map_or(Color::WHITE, |c| Color::srgb_u8(c[0], c[1], c[2]))
}

fn material_for(part: &HumanPart, assets: &mut HumanAssets, tints: Tints) -> StandardMaterial {
    use cartopolis_human::ProxyKind;
    let texture = part.texture.as_ref().map(|t| assets.texture(t));
    let mut m = StandardMaterial {
        base_color: base_colour(part, tints),
        base_color_texture: texture,
        perceptual_roughness: 0.75,
        reflectance: 0.35,
        ..default()
    };
    if let PartKind::Proxy(kind) = part.kind {
        if part.alpha {
            m.alpha_mode = AlphaMode::Mask(0.5);
            m.double_sided = true;
            m.cull_mode = None;
        }
        // Eyes are a glossy ball.
        if kind == ProxyKind::Eyes {
            m.perceptual_roughness = 0.2;
        }
    }
    m
}

/// A retargeted clip as an `AnimationClip`: a rotation curve per joint, and the
/// pelvis's translation.
fn animation_clip(
    clip: &cartopolis_human::RetargetedClip,
    model: &HumanModel,
    paths: &[Vec<Name>],
) -> AnimationClip {
    let mut out = AnimationClip::default();
    let Ok(domain) = Interval::new(0.0, clip.duration().max(1.0 / clip.fps)) else {
        return out;
    };
    for (i, frames) in clip.rotations.iter().enumerate() {
        if frames.len() < 2 || i >= model.joints.len() {
            continue;
        }
        let target = AnimationTargetId::from_names(paths[i].iter());
        if let Ok(curve) = SampleAutoCurve::new(domain, frames.iter().map(|q| Quat::from_array(*q)))
        {
            out.add_curve_to_target(
                target,
                AnimatableCurve::new(animated_field!(Transform::rotation), curve),
            );
        }
    }
    if let Some((p, at)) = &clip.pelvis {
        if at.len() >= 2 {
            let target = AnimationTargetId::from_names(paths[*p].iter());
            if let Ok(curve) = SampleAutoCurve::new(domain, at.iter().map(|v| Vec3::from_array(*v)))
            {
                out.add_curve_to_target(
                    target,
                    AnimatableCurve::new(animated_field!(Transform::translation), curve),
                );
            }
        }
    }
    out
}

/// The shortest gap between two rebuilds started while a look changes. The
/// build itself runs on a worker, so this is not about the frame: it bounds
/// how much mesh a slider drag uploads, about 1.5 MB a body.
const REBUILD_SECS: f32 = 0.1;

/// How many frames a rebuilt body waits, hidden, after its textures are in,
/// before it replaces the old one: its meshes go up on the render thread, a
/// frame behind, and a body shown before they have is not drawn at all — the
/// flicker this whole arrangement exists to prevent.
const SWAP_FRAMES: u8 = 2;

/// An avatar whose body is being built again in a new look. The old body
/// stays on screen until the new one can be drawn whole ([`finish_rebuilds`]).
///
/// Rebuilding used to despawn the body and let `spawn_avatar` build the next
/// one on the following frame: every slider step left frames with no body —
/// none at all until the new meshes were up, and a white one until any
/// texture dropped with the old materials had been loaded and encoded again —
/// and restarted the clip from its first frame: on the designer's sliders, a
/// body flickering as it was dragged (reported 2026-10-19).
#[derive(Component)]
pub(crate) enum HumanRebuild {
    /// Being built on a worker.
    Building {
        receiver: futures_channel::oneshot::Receiver<PreparedHuman>,
        tints: Tints,
    },
    /// Spawned with its meshes hidden, waiting for its textures; `ready`
    /// counts the frames since they arrived.
    Showing {
        holder: Entity,
        anims: super::avatar::AvatarAnimations,
        ready: u8,
    },
}

/// Build `look` for `avatar` on a worker; [`finish_rebuilds`] takes it from
/// there.
fn start_rebuild(commands: &mut Commands, avatar: Entity, pack: Arc<HumanPack>, look: &HumanLook) {
    use crate::platform::workers;
    let (look, tints) = look_for(look);
    let (done, receiver) = futures_channel::oneshot::channel();
    // Near: it is a body on screen, being looked at.
    workers::submit_ranked(
        "avatar body",
        workers::NEAR,
        Box::new(move || {
            let _ = done.send(prepare_human(&pack, &look));
        }),
    );
    commands
        .entity(avatar)
        .try_insert(HumanRebuild::Building { receiver, tints });
}

/// Whether two looks are the same body in the same clothes, whatever their
/// colours: a colour is a material's base colour over a neutral texture, so a
/// change of colour alone repaints the body rather than building it again.
fn same_body(a: &HumanLook, b: &HumanLook) -> bool {
    let shape = |l: &HumanLook| HumanLook {
        hair_colour: None,
        top_colour: None,
        bottom_colour: None,
        ..l.clone()
    };
    shape(a) == shape(b)
}

/// Rebuild an avatar's MakeHuman body when the look it should wear changes —
/// a slider moved in the designer, a look saved or abandoned, another
/// player's look arriving — or repaint it, when only a colour changed.
///
/// One rebuild per avatar at a time: a look that changes while its body is
/// being built is started once that build is done, so a drag ends on the look
/// it stopped at.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rebuild_on_look_change(
    mut commands: Commands,
    store: Res<crate::systems::user_store::UserStore>,
    draft: Res<HumanDraft>,
    mut remote_looks: ResMut<RemoteLooks>,
    handle: Option<Res<HumanPackHandle>>,
    packs: Res<Assets<HumanPackAsset>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut textures: ResMut<HumanTextures>,
    time: Res<Time>,
    children: Query<&Children>,
    parts: Query<(&HumanPart, &MeshMaterial3d<StandardMaterial>)>,
    avatars: Query<
        (Entity, Has<HumanRebuild>),
        (
            With<crate::components::Avatar>,
            With<crate::components::AvatarModelLoaded>,
        ),
    >,
    remotes: Query<
        (Entity, &crate::components::RemoteAvatar, Has<HumanRebuild>),
        With<crate::components::AvatarModelLoaded>,
    >,
    mut last: Local<Option<HumanLook>>,
    mut since: Local<f32>,
) {
    // Closing the designer lets go of the textures it was keeping.
    if draft.0.is_none() && !textures.0.is_empty() {
        textures.0.clear();
    }
    let Some(pack) = handle
        .as_ref()
        .and_then(|h| packs.get(&h.0))
        .map(|p| p.0.clone())
    else {
        return;
    };

    // Another avatar's look arrived after its model was built (it changed it,
    // or the look came after the snapshot): build it again in the new one.
    if !remote_looks.changed.is_empty() {
        let mut later = std::collections::HashSet::new();
        for (entity, remote, rebuilding) in &remotes {
            if !remote_looks.changed.contains(&remote.sim_id) {
                continue;
            }
            if rebuilding {
                later.insert(remote.sim_id);
            } else {
                let look = remote_looks.look_of(remote.sim_id);
                start_rebuild(&mut commands, entity, pack.clone(), &look);
            }
        }
        // An avatar not built yet will be built in its latest look anyway.
        remote_looks.changed = later;
    }

    *since += time.delta_secs();
    let want = current_look(&store.data.appearance, &draft);
    if last.as_ref() == Some(&want) {
        return;
    }
    // The first look is the one the avatar was built with; so is any look
    // that changes before it has been.
    let Some(was) = last.clone().filter(|_| !avatars.is_empty()) else {
        *last = Some(want);
        return;
    };
    if avatars.iter().any(|(_, rebuilding)| rebuilding) {
        return;
    }
    if same_body(&was, &want) {
        let (_, tints) = look_for(&want);
        for (avatar, _) in &avatars {
            for e in children.iter_descendants(avatar) {
                let Ok((part, material)) = parts.get(e) else {
                    continue;
                };
                let colour = base_colour(part, tints);
                // Read first: `get_mut` re-prepares the material even when
                // nothing changed.
                if materials
                    .get(&material.0)
                    .is_some_and(|m| m.base_color != colour)
                {
                    if let Some(mut m) = materials.get_mut(&material.0) {
                        m.base_color = colour;
                    }
                }
            }
        }
        *last = Some(want);
        return;
    }
    if *since < REBUILD_SECS {
        return;
    }
    *since = 0.0;
    for (avatar, _) in &avatars {
        start_rebuild(&mut commands, avatar, pack.clone(), &want);
    }
    *last = Some(want);
}

/// Bring rebuilt bodies in: spawn each, hidden, as it comes back from its
/// worker, starting its clip where the old body's is; once its textures are
/// in and its meshes have had [`SWAP_FRAMES`] to reach the GPU, show it and
/// drop the old one in the same frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_rebuilds(
    mut commands: Commands,
    mut rebuilds: Query<(Entity, &mut HumanRebuild)>,
    mut human: HumanAssets,
    mut clip_assets: ResMut<Assets<AnimationClip>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    children: Query<&Children>,
    holders: Query<&Visibility, With<super::avatar::AvatarHolder>>,
    players: Query<(&AnimationPlayer, &AnimationTransitions)>,
    parts: Query<&MeshMaterial3d<StandardMaterial>, With<HumanPart>>,
) {
    for (avatar, mut rebuild) in &mut rebuilds {
        let old_holders = || {
            children
                .get(avatar)
                .into_iter()
                .flatten()
                .copied()
                .filter(|e| holders.contains(*e))
        };
        match &mut *rebuild {
            HumanRebuild::Building { receiver, tints } => {
                let prepared = match receiver.try_recv() {
                    Ok(Some(p)) => p,
                    Ok(None) => continue,
                    // The worker dropped it (a panic in the build): keep the
                    // body there is.
                    Err(_) => {
                        commands.entity(avatar).try_remove::<HumanRebuild>();
                        continue;
                    }
                };
                let old = old_holders().next();
                let spawned = spawn_human(
                    &mut commands,
                    &mut human,
                    &mut clip_assets,
                    prepared,
                    *tints,
                    avatar,
                    super::avatar::AvatarHolder,
                    true,
                );
                let holder = spawned.holder;
                // Hidden or shown as the old one is: the first-person and
                // driver systems own a holder's visibility.
                if let Some(v) = old.and_then(|o| holders.get(o).ok()) {
                    commands.entity(holder).insert(*v);
                }
                let anims = super::avatar::animate_holder(
                    &mut commands,
                    &mut graphs,
                    spawned,
                    old.and_then(|o| players.get(o).ok()),
                );
                *rebuild = HumanRebuild::Showing {
                    holder,
                    anims,
                    ready: 0,
                };
            }
            HumanRebuild::Showing {
                holder,
                anims,
                ready,
            } => {
                let holder = *holder;
                if !holders.contains(holder) {
                    commands.entity(avatar).try_remove::<HumanRebuild>();
                    continue;
                }
                let Ok(meshes) = children.get(holder) else {
                    continue;
                };
                let loaded = meshes.iter().filter_map(|e| parts.get(e).ok()).all(|m| {
                    human
                        .materials
                        .get(&m.0)
                        .and_then(|m| m.base_color_texture.as_ref())
                        .is_none_or(|t| {
                            human.asset_server.is_loaded_with_dependencies(t)
                                || matches!(
                                    human.asset_server.load_state(t),
                                    bevy::asset::LoadState::Failed(_)
                                )
                        })
                });
                if !loaded {
                    continue;
                }
                *ready = ready.saturating_add(1);
                if *ready <= SWAP_FRAMES {
                    continue;
                }
                for e in meshes.iter().filter(|e| parts.contains(*e)) {
                    commands.entity(e).try_insert(Visibility::Inherited);
                }
                for old in old_holders().filter(|e| *e != holder) {
                    commands.entity(old).try_despawn();
                }
                commands
                    .entity(avatar)
                    .try_insert(anims.clone())
                    .try_remove::<HumanRebuild>();
            }
        }
    }
}

/// Other avatars' looks by sim id, as the server sends them (protocol 42).
///
/// A resource rather than a field on `RemoteAvatar`, because a look can arrive
/// before the snapshot that spawns its avatar: the server hands a newcomer
/// every look first, then the world.
#[derive(Resource, Default)]
pub(crate) struct RemoteLooks {
    by_id: std::collections::HashMap<u64, HumanLook>,
    /// Avatars whose look arrived after their model was built, to rebuild.
    changed: std::collections::HashSet<u64>,
}

impl RemoteLooks {
    pub(crate) fn arrived(&mut self, avatar_id: u64, look: HumanLook) {
        if self.by_id.get(&avatar_id) != Some(&look) {
            self.by_id.insert(avatar_id, look);
            self.changed.insert(avatar_id);
        }
    }

    /// The look avatar `id` wears: what the server sent, else the default
    /// body, which is also what its owner sees until they design one.
    pub(crate) fn look_of(&self, id: u64) -> HumanLook {
        self.by_id
            .get(&id)
            .cloned()
            .unwrap_or_else(|| HumanLook::default_for(false))
    }
}

/// Tell the server this client's look (protocol 42): once on every new
/// connection, and again whenever the saved look changes. Nothing while the
/// avatar has never been designed — the server keeps no look then, and others
/// draw the default body, as this client does.
pub(crate) fn send_avatar_look(
    online: Option<Res<crate::resources::OnlineSession>>,
    store: Res<crate::systems::user_store::UserStore>,
    mut sent: Local<Option<HumanLook>>,
) {
    let Some(online) = online else {
        *sent = None;
        return;
    };
    if online.is_added() {
        *sent = None;
    }
    let Some(look) = store.data.appearance.human.as_ref() else {
        return;
    };
    if sent.as_ref() == Some(look) {
        return;
    }
    if online.send(
        "avatar look",
        cartopolis_core::NetMessage::SetAvatarLook { look: look.clone() },
    ) {
        *sent = Some(look.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A look is a rebuild only when it is new: the same look again (a client
    /// re-sends on every connection) changes nothing.
    #[test]
    fn only_a_new_look_rebuilds() {
        let mut looks = RemoteLooks::default();
        let look = HumanLook::default_for(true);
        looks.arrived(7, look.clone());
        assert!(looks.changed.contains(&7));
        looks.changed.clear();
        looks.arrived(7, look.clone());
        assert!(
            looks.changed.is_empty(),
            "the same look again is not a change"
        );
        assert_eq!(looks.look_of(7), look);
        assert_eq!(
            looks.look_of(8),
            HumanLook::default_for(false),
            "no look yet: the default body"
        );
    }

    #[test]
    fn age_and_skin_follow_the_body() {
        assert_eq!(age_value(25), 0.5);
        assert!((age_value(90) - 1.0).abs() < 1.0e-6);
        assert!(
            age_value(18) > 0.3 && age_value(18) < 0.5,
            "18 blends MakeHuman's child and young"
        );
        assert_eq!(skin_name("african", 30, 10), "young_african_female");
        assert_eq!(skin_name("asian", 70, 90), "old_asian_male");
    }
}

//! The MakeHuman avatar: a person built at runtime from `cartopolis_human`'s
//! pack instead of a Quaternius character file (`docs/notes/makehuman-avatars.md`).
//!
//! **Behind `CARTO_HUMAN_AVATAR=1`, local avatar only**, while it is being
//! judged against the characters it would replace; remote avatars and the
//! default stay Quaternius.
//!
//! The model is not a glTF scene: the joints, meshes and clips are made here
//! from a [`HumanModel`], and slotted under the avatar as one holder that
//! [`avatar`](super::avatar) drives like any other — the same
//! `AvatarAnimations` table, the same pose and lean systems. What it has to
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
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use cartopolis_human::{HumanModel, HumanPack, Look, PartKind, Phenotype};

use crate::systems::avatar::ClipGait;
use crate::systems::user_store::{AvatarAppearance, HumanLook};

/// Where the pack and its textures live under `assets/`.
const PACK_DIR: &str = "models/avatar/human";

/// The switch that puts a MakeHuman body on the local avatar.
pub(crate) fn enabled() -> bool {
    crate::debug_switches::env_flag("CARTO_HUMAN_AVATAR")
}

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

#[derive(Resource)]
pub(crate) struct HumanPackHandle(Handle<HumanPackAsset>);

/// Start loading the pack, if the switch is on.
pub(crate) fn load_human_pack(mut commands: Commands, asset_server: Res<AssetServer>) {
    if enabled() {
        commands.insert_resource(HumanPackHandle(
            asset_server.load(format!("{PACK_DIR}/default.humanpack")),
        ));
    }
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
    packs: Res<'w, Assets<HumanPackAsset>>,
    asset_server: Res<'w, AssetServer>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
}

impl HumanAssets<'_> {
    /// The look the local avatar should be built in now ([`current_look`]).
    pub(crate) fn look(&self, appearance: &AvatarAppearance) -> (Look, Tints) {
        look_for(&current_look(appearance, &self.draft))
    }

    /// The pack, once it has loaded.
    pub(crate) fn pack(&self) -> Option<Arc<HumanPack>> {
        let h = self.handle.as_ref()?;
        self.packs.get(&h.0).map(|p| p.0.clone())
    }

    /// The pack will never load (missing, an LFS pointer, an old layout). The
    /// avatar then falls back to a character file rather than waiting, which
    /// is what it did until 2026-10-17: invisible for the whole session.
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
        .unwrap_or_else(|| with_overrides(HumanLook::default_for(appearance.sex)))
}

/// `CARTO_HUMAN_LOOK="body_shape=0,weight=90,age=70"`: start an undesigned avatar
/// with these sliders, so a capture — which cannot drag one — can photograph a
/// body other than the default. Unknown keys and bad numbers are ignored.
fn with_overrides(mut look: HumanLook) -> HumanLook {
    let Some(spec) = crate::debug_switches::env_value("CARTO_HUMAN_LOOK") else {
        return look;
    };
    for pair in spec.split(',') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let Ok(v) = v.trim().parse::<u8>() else {
            continue;
        };
        match k.trim() {
            "body_shape" => look.body_shape = v.min(100),
            "age" => look.age = v.clamp(18, 90),
            "height" => look.height = v.min(100),
            "weight" => look.weight = v.min(100),
            "muscle" => look.muscle = v.min(100),
            "proportions" => look.proportions = v.min(100),
            "chest" => look.chest = v.min(100),
            _ => {}
        }
    }
    if look.body_shape < 50 {
        look.outfit = "female_casualsuit01".into();
    }
    look
}

/// Colours multiplied over the textures: a tone over the one detailed skin, a
/// tint over the hair and the outfit, so the folds and strands stay.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tints {
    pub skin: Color,
    pub hair: Color,
    pub outfit: Color,
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
    // The pack's two skins differ in more than colour, so the nearer one.
    let skin = if h.body_shape >= 50 { "male" } else { "female" };
    let mut proxies: Vec<String> = ["high-poly", "eyebrow001", "eyelashes01"]
        .map(String::from)
        .to_vec();
    proxies.extend(h.hair.clone());
    proxies.push(h.outfit.clone());
    proxies.push("shoes06".into());
    let rgb = |c: [u8; 3]| Color::srgb_u8(c[0], c[1], c[2]);
    (
        Look {
            phenotype,
            skin: skin.into(),
            proxies,
        },
        Tints {
            skin: rgb(h.skin_tone),
            hair: rgb(h.hair_colour),
            outfit: rgb(h.outfit_colour),
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

/// Build `look` and put it under `parent` as one holder. `holder` bundles what
/// the caller's avatar code tags a holder with.
pub(crate) fn spawn_human(
    commands: &mut Commands,
    assets: &mut HumanAssets,
    clip_assets: &mut Assets<AnimationClip>,
    pack: &HumanPack,
    look: &Look,
    tints: Tints,
    parent: Entity,
    holder: impl Bundle,
) -> SpawnedHuman {
    let started = bevy::platform::time::Instant::now();
    let model = cartopolis_human::build(pack, look);
    let built_ms = started.elapsed().as_secs_f32() * 1000.0;

    let holder_name = Name::new("human");
    let holder = commands
        .spawn((
            holder,
            holder_name.clone(),
            Transform::default(),
            Visibility::default(),
            ChildOf(parent),
        ))
        .id();

    // Joints, parents first (the model's order), each bound to the holder's
    // player by its name path.
    let mut paths: Vec<Vec<Name>> = Vec::with_capacity(model.joints.len());
    let mut joints: Vec<Entity> = Vec::with_capacity(model.joints.len());
    for j in &model.joints {
        let mut path = match j.parent {
            Some(p) => paths[p].clone(),
            None => vec![holder_name.clone()],
        };
        path.push(Name::new(j.name.clone()));
        let entity = commands
            .spawn((
                Name::new(j.name.clone()),
                Transform {
                    translation: Vec3::from_array(j.translation),
                    rotation: Quat::from_array(j.rotation),
                    scale: Vec3::ONE,
                },
                Visibility::default(),
                AnimationTargetId::from_names(path.iter()),
                AnimatedBy(holder),
                ChildOf(j.parent.map_or(holder, |p| joints[p])),
            ))
            .id();
        paths.push(path);
        joints.push(entity);
    }
    let bindposes = assets.bindposes.add(SkinnedMeshInverseBindposes::from(
        model
            .joints
            .iter()
            .map(|j| Mat4::from_cols_array(&j.inverse_bind))
            .collect::<Vec<_>>(),
    ));

    let mut vertices = 0;
    for part in &model.parts {
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
        let material = material_for(part, &assets.asset_server, tints);
        commands.spawn((
            Name::new(part.name.clone()),
            Mesh3d(assets.meshes.add(mesh)),
            MeshMaterial3d(assets.materials.add(material)),
            SkinnedMesh {
                inverse_bindposes: bindposes.clone(),
                joints: joints.clone(),
            },
            Transform::default(),
            Visibility::default(),
            ChildOf(holder),
        ));
    }

    let find = |name: &str| model.clips.iter().find(|c| c.name == name);
    let idle = clip_assets.add(
        find("idle")
            .map(|c| animation_clip(c, &model, &paths))
            .unwrap_or_default(),
    );
    let clips = CLIP_ORDER
        .iter()
        .map(|name| match find(name) {
            Some(c) if *name != "idle" => clip_assets.add(animation_clip(c, &model, &paths)),
            _ => idle.clone(),
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
    let walk = gait("walk", ClipGait::CHARACTER_WALK);
    let run = gait("run", ClipGait::CHARACTER_RUN);
    // A gesture the pack lacks plays the idle, for as long as the characters'
    // emotes last.
    let emote_secs = CLIP_ORDER[3..9]
        .iter()
        .map(|n| find(n).map_or(1.6, |c| c.duration()))
        .collect();

    tracing::info!(
        height_m = model.height,
        joints = model.joints.len(),
        parts = model.parts.len(),
        vertices,
        built_ms,
        missing_targets = model.missing_targets.len(),
        "avatar: MakeHuman body built"
    );
    SpawnedHuman {
        holder,
        clips,
        walk,
        run,
        emote_secs,
    }
}

fn material_for(
    part: &cartopolis_human::Part,
    asset_server: &AssetServer,
    tints: Tints,
) -> StandardMaterial {
    use cartopolis_human::ProxyKind;
    let texture = part
        .texture
        .as_ref()
        .map(|t| asset_server.load(format!("{PACK_DIR}/textures/{t}")));
    let mut m = StandardMaterial {
        base_color_texture: texture,
        perceptual_roughness: 0.75,
        reflectance: 0.35,
        ..default()
    };
    match part.kind {
        PartKind::Body => m.base_color = tints.skin,
        // Eyes are a glossy ball; cut out like the rest of the proxies because
        // their cornea layer is transparent around the iris.
        PartKind::Proxy(kind) => {
            if part.alpha {
                m.alpha_mode = AlphaMode::Mask(0.5);
                m.double_sided = true;
                m.cull_mode = None;
            }
            match kind {
                ProxyKind::Eyes => m.perceptual_roughness = 0.2,
                ProxyKind::Hair | ProxyKind::Eyebrows | ProxyKind::Eyelashes => {
                    m.base_color = tints.hair;
                }
                // The shoes keep their own colour; the outfit takes the tint.
                ProxyKind::Clothes if !part.name.starts_with("shoes") => {
                    m.base_color = tints.outfit;
                }
                ProxyKind::Clothes => {}
            }
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

/// The shortest gap between two rebuilds of the avatar while a look changes.
/// A rebuild is a few milliseconds of shaping and fitting, then every mesh
/// uploaded again (about 1.5 MB), so a slider drag rebuilds several times a
/// second rather than every frame.
const REBUILD_SECS: f32 = 0.15;

/// Rebuild the local MakeHuman avatar when the look it should wear changes —
/// a slider moved in the designer, a look saved or abandoned.
///
/// The rebuild is `spawn_avatar`'s own: the avatar's model is despawned and its
/// markers dropped, and the next frame builds it fresh from [`current_look`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn rebuild_on_look_change(
    mut commands: Commands,
    store: Res<crate::systems::user_store::UserStore>,
    draft: Res<HumanDraft>,
    time: Res<Time>,
    children: Query<&Children>,
    avatars: Query<
        Entity,
        (
            With<crate::components::Avatar>,
            With<crate::components::AvatarModelLoaded>,
        ),
    >,
    mut last: Local<Option<HumanLook>>,
    mut since: Local<f32>,
) {
    if !enabled() {
        return;
    }
    *since += time.delta_secs();
    let want = current_look(&store.data.appearance, &draft);
    if last.as_ref() == Some(&want) {
        return;
    }
    // The first look is the one the avatar was built with.
    if last.is_none() {
        *last = Some(want);
        return;
    }
    if *since < REBUILD_SECS {
        return;
    }
    *since = 0.0;
    *last = Some(want);
    for entity in &avatars {
        let model: Vec<Entity> = children.iter_descendants(entity).collect();
        for e in model {
            commands.entity(e).try_despawn();
        }
        commands.entity(entity).try_remove::<(
            crate::components::AvatarModelLoaded,
            super::avatar::AvatarAnimations,
        )>();
    }
}

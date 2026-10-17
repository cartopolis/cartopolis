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

use crate::systems::user_store::AvatarSex;

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

/// The library clips the avatar's table is built from, in its order: idle,
/// walk, run, the six emotes in `EMOTE_CLIPS` order (wave, interact, roll,
/// punch, kick, faint), and sitting. The library has no wave and no kick; the
/// nearest gestures stand in.
pub(crate) const CLIP_ORDER: [&str; 10] = [
    "Idle_Loop",
    "Walk_Loop",
    "Jog_Fwd_Loop",
    "Idle_Talking_Loop",
    "Interact",
    "Roll",
    "Punch_Jab",
    "Punch_Cross",
    "Death01",
    "Sitting_Idle_Loop",
];

/// What the spawn needs to build a person.
#[derive(SystemParam)]
pub(crate) struct HumanAssets<'w> {
    handle: Option<Res<'w, HumanPackHandle>>,
    packs: Res<'w, Assets<HumanPackAsset>>,
    asset_server: Res<'w, AssetServer>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
}

impl HumanAssets<'_> {
    /// The pack, once it has loaded.
    pub(crate) fn pack(&self) -> Option<Arc<HumanPack>> {
        let h = self.handle.as_ref()?;
        self.packs.get(&h.0).map(|p| p.0.clone())
    }
}

/// The look this client gives a body of `sex`, until there is a chooser.
pub(crate) fn default_look(sex: AvatarSex) -> Look {
    let (phenotype, skin, outfit) = match sex {
        AvatarSex::Man => (Phenotype::male(), "male", "male_casualsuit06"),
        AvatarSex::Woman => (Phenotype::female(), "female", "female_casualsuit01"),
    };
    Look {
        phenotype,
        skin: skin.into(),
        proxies: [
            "high-poly",
            "eyebrow001",
            "eyelashes01",
            "short02",
            outfit,
            "shoes06",
        ]
        .map(String::from)
        .to_vec(),
    }
}

/// A spawned person: the holder entity (to be given the player and the
/// animation table) and the clips in [`CLIP_ORDER`].
pub(crate) struct SpawnedHuman {
    pub holder: Entity,
    pub clips: Vec<Handle<AnimationClip>>,
}

/// Build `look` and put it under `parent` as one holder. `holder` bundles what
/// the caller's avatar code tags a holder with.
pub(crate) fn spawn_human(
    commands: &mut Commands,
    assets: &mut HumanAssets,
    clip_assets: &mut Assets<AnimationClip>,
    pack: &HumanPack,
    look: &Look,
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
        let material = material_for(part, &assets.asset_server);
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

    let clips = CLIP_ORDER
        .iter()
        .map(|name| {
            let clip = model
                .clips
                .iter()
                .find(|c| c.name == *name)
                .map(|c| animation_clip(c, &model, &paths))
                .unwrap_or_default();
            clip_assets.add(clip)
        })
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
    SpawnedHuman { holder, clips }
}

fn material_for(part: &cartopolis_human::Part, asset_server: &AssetServer) -> StandardMaterial {
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
        PartKind::Body => {}
        // Eyes are a glossy ball; cut out like the rest of the proxies because
        // their cornea layer is transparent around the iris.
        PartKind::Proxy(kind) => {
            if part.alpha {
                m.alpha_mode = AlphaMode::Mask(0.5);
                m.double_sided = true;
                m.cull_mode = None;
            }
            if kind == cartopolis_human::ProxyKind::Eyes {
                m.perceptual_roughness = 0.2;
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

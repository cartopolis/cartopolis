//! A pack and a look become a [`HumanModel`]: meshes, a skeleton, clips.
//!
//! Done once per person, when they appear or change how they look — never per
//! frame. The shaping and the fitting are a few passes over ~20 000 vertices;
//! after that the GPU skins the meshes and the CPU only poses 53 bones.

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::{fit, macros, rig, Face, HumanPack, Phenotype, Proxy, ProxyKind, BODY_VERTICES};

/// How a person looks: the body's sliders, a skin, and what they wear (proxy
/// names in the pack).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Look {
    pub phenotype: Phenotype,
    pub skin: String,
    pub proxies: Vec<String>,
    /// An eye colour from [`HumanPack::eye_colours`], over the eyes' own.
    #[serde(default)]
    pub eyes: Option<String>,
    /// Face controls (`HumanPack::modifiers`) by name, each -1 … 1.
    #[serde(default)]
    pub modifiers: Vec<(String, f32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Body,
    Proxy(ProxyKind),
}

/// One drawable mesh, skinned to [`HumanModel::joints`]. Metres, our frame,
/// UVs with v down (wgpu's convention).
#[derive(Clone, Debug)]
pub struct Part {
    pub kind: PartKind,
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub texture: Option<String>,
    pub alpha: bool,
    /// Which of the proxy's tint regions this is (an outfit's top 0, bottom 1).
    pub region: u8,
    /// The colour that shows a neutral texture as authored; `None` when the
    /// texture is the original and takes no tint.
    pub default_tint: Option<[u8; 3]>,
    /// Its normal map, when the pack has one (linear: directions, not colour).
    pub normal: Option<String>,
    /// One per face unit ([`crate::face::UNITS`]) on a part the face's units
    /// move — the face, the eyes, the brows, the lashes; empty on the rest.
    pub morphs: Vec<Morph>,
}

/// What one face unit at full weight does to a part: per vertex, the change
/// of its position and of its normal, metres and our frame, before skinning.
#[derive(Clone, Debug, Default)]
pub struct Morph {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
}

/// One bone, as an engine wants it: rest transform relative to its parent, and
/// the inverse of its rest transform relative to the model.
#[derive(Clone, Debug)]
pub struct Joint {
    pub name: String,
    pub parent: Option<usize>,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub inverse_bind: [f32; 16],
}

/// A library clip played on this body: per joint, its local rotation at each
/// frame; and the pelvis's local translation.
#[derive(Clone, Debug)]
pub struct RetargetedClip {
    pub name: String,
    pub fps: f32,
    /// Ground speed the clip was captured at, metres a second on this body; 0
    /// for a clip played on the spot.
    pub speed: f32,
    pub looped: bool,
    pub rotations: Vec<Vec<[f32; 4]>>,
    pub pelvis: Option<(usize, Vec<[f32; 3]>)>,
}

impl RetargetedClip {
    /// Seconds from the first sample to the last.
    #[must_use]
    pub fn duration(&self) -> f32 {
        let frames = self.rotations.first().map_or(0, Vec::len);
        frames.saturating_sub(1) as f32 / self.fps
    }
}

#[derive(Clone, Debug)]
pub struct HumanModel {
    pub parts: Vec<Part>,
    pub joints: Vec<Joint>,
    pub clips: Vec<RetargetedClip>,
    /// Each hand closed round a phone, left first (`hands::phone_grip`).
    pub phone_grip: [Option<crate::hands::PhoneGrip>; 2],
    /// Top of the head above the soles, metres.
    pub height: f32,
    /// Targets the phenotype asked for that the pack does not carry — a pack
    /// cut down to a few bodies, asked for another.
    pub missing_targets: Vec<String>,
}

/// Rounds of [`fit::smooth_weights`] over a garment: each round reaches one
/// edge further across the garment's mesh.
const GARMENT_SMOOTHING: usize = 8;

/// Build the person `look` describes.
#[must_use]
pub fn build(pack: &HumanPack, look: &Look) -> HumanModel {
    // Shape, in the OBJ frame the targets and the fitting rules are written in.
    let mut base: Vec<Vec3> = pack.vertices.iter().map(|&v| Vec3::from_array(v)).collect();
    let mut missing_targets = Vec::new();
    let by_name: HashMap<&str, &crate::Target> =
        pack.targets.iter().map(|t| (t.name.as_str(), t)).collect();
    // The face controls on top of the body: each picks its negative or its
    // positive targets by sign, at the slider's size.
    let mut wanted = macros::macro_targets(&look.phenotype);
    for (name, v) in &look.modifiers {
        let Some(m) = pack.modifiers.iter().find(|m| &m.name == name) else {
            missing_targets.push(name.clone());
            continue;
        };
        let side = if *v < 0.0 { &m.negative } else { &m.positive };
        let w = v.abs().min(1.0);
        if w > 0.0 {
            wanted.extend(side.iter().map(|t| (t.clone(), w)));
        }
    }
    for (name, w) in wanted {
        match by_name.get(name.as_str()) {
            Some(t) => {
                for (i, d) in t.offsets() {
                    base[i] += w * d;
                }
            }
            None => missing_targets.push(name),
        }
    }
    let proxies: Vec<&Proxy> = look
        .proxies
        .iter()
        .filter_map(|n| pack.proxies.iter().find(|p| &p.name == n))
        .collect();
    let fitted: Vec<Vec<Vec3>> = proxies.iter().map(|p| fit::fit(p, &base)).collect();

    // Metres, standing on y = 0.
    let floor = base[..BODY_VERTICES.min(base.len())]
        .iter()
        .map(|v| v.y)
        .fold(f32::INFINITY, f32::min);
    let to_metres = |v: Vec3| (v - Vec3::Y * floor) * 0.1;
    let positions: Vec<Vec3> = base.iter().map(|&v| to_metres(v)).collect();
    let height = positions[..BODY_VERTICES.min(positions.len())]
        .iter()
        .map(|v| v.y)
        .fold(0.0, f32::max);

    // The skeleton, placed in this body.
    let rest = rig::rest_pose(pack, &positions);
    let tpose = rig::tpose_world(pack, &rest);
    let joints: Vec<Joint> = pack
        .bones
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let parent = b.parent.map(usize::from);
            let (translation, rotation) = match parent {
                Some(p) => (
                    rest[p].rotation.inverse() * (rest[i].head - rest[p].head),
                    (rest[p].rotation.inverse() * rest[i].rotation).normalize(),
                ),
                None => (rest[i].head, rest[i].rotation),
            };
            let bind = Mat4::from_rotation_translation(rest[i].rotation, rest[i].head);
            Joint {
                name: b.name.clone(),
                parent,
                translation: translation.to_array(),
                rotation: rotation.to_array(),
                inverse_bind: bind.inverse().to_cols_array(),
            }
        })
        .collect();

    // The skin, less what the clothes cover: what each lists in
    // `delete_verts`, and for separates — whose community authors mostly
    // left that list empty, so the chest showed through a sweater — the
    // base vertices they hang on (a body face goes only when all its corners
    // do, so nothing opens at a hem).
    let separates_cover = close_over(
        proxies
            .iter()
            .filter(|p| matches!(p.kind, ProxyKind::Top | ProxyKind::Bottom))
            .flat_map(|p| p.refs.iter().flat_map(|r| r.v))
            .collect(),
        &pack.body_faces,
    );
    let covered: HashSet<u32> = proxies
        .iter()
        .flat_map(|p| p.delete_verts.iter().copied())
        .chain(separates_cover)
        .collect();
    let body_faces: Vec<Face> = pack
        .body_faces
        .iter()
        .filter(|f| !corners(f).all(|v| covered.contains(&v)))
        .copied()
        .collect();
    // The skin a worn top lies over, to tuck a bottom under it: the base
    // vertices its own vertices hang on, grown by one ring of the body's
    // faces so the gaps between them close. Not its `delete_verts`, which
    // most tops leave empty (the sweater does).
    let top_covers: HashSet<u32> = close_over(
        proxies
            .iter()
            .filter(|p| p.kind == ProxyKind::Top)
            .flat_map(|p| p.refs.iter().flat_map(|r| r.v))
            .collect(),
        &pack.body_faces,
    );
    let skin = pack.skins.iter().find(|s| s.name == look.skin);
    // The face's units, metres. They are offsets on the base mesh, the same
    // for every body, as MakeHuman applies its expressions.
    let units: Vec<Vec<(usize, Vec3)>> = pack
        .face_units
        .iter()
        .map(|t| t.offsets().map(|(i, d)| (i, d * 0.1)).collect())
        .collect();
    // The skin the units move is drawn as a part of its own, the face, so
    // that only its few thousand vertices carry 52 morph targets: the engine
    // stores a target's change for every vertex of a mesh, moved or not.
    // Normals are summed over the whole skin first, so the seam between the
    // two parts does not show.
    let moved: HashSet<u32> = units
        .iter()
        .flatten()
        .map(|(i, _)| *i as u32)
        .filter(|&i| (i as usize) < BODY_VERTICES)
        .collect();
    let (face_faces, rest_faces): (Vec<Face>, Vec<Face>) = body_faces
        .iter()
        .partition(|f| corners(f).any(|v| moved.contains(&v)));
    let body_normals = normal_sums(&positions, &body_faces);
    let skin_part = |name: &str, faces: &[Face]| {
        mesh_part(
            PartKind::Body,
            name,
            &positions,
            &body_normals,
            &pack.uvs,
            faces,
            |v| pack.weights[v].clone(),
            skin.map(|s| s.texture.clone()),
            false,
        )
    };
    let mut parts = vec![skin_part("body", &rest_faces).0];
    if !face_faces.is_empty() {
        let (mut face, source) = skin_part("face", &face_faces);
        face.morphs = morphs(&positions, &body_normals, &face_faces, &units, &source);
        parts.push(face);
    }
    let eye_texture = look
        .eyes
        .as_ref()
        .and_then(|e| pack.eye_colours.iter().find(|c| &c.name == e))
        .map(|c| c.texture.clone());
    for (proxy, fitted) in proxies.iter().zip(&fitted) {
        let at: Vec<Vec3> = fitted.iter().map(|&v| to_metres(v)).collect();
        let texture = match proxy.kind {
            ProxyKind::Eyes => eye_texture.clone().or_else(|| proxy.texture.clone()),
            _ => proxy.texture.clone(),
        };
        // Skin weights from the body, evened out across what is worn, so
        // neighbours on a garment do not pull apart (`fit::smooth_weights`).
        let mut weights: Vec<Vec<(u16, f32)>> = proxy
            .refs
            .iter()
            .map(|r| fit::blend_weights(r, &pack.weights))
            .collect();
        if matches!(
            proxy.kind,
            ProxyKind::Outfit
                | ProxyKind::Top
                | ProxyKind::Bottom
                | ProxyKind::Shoes
                | ProxyKind::Hat
                | ProxyKind::Glasses
        ) {
            let edges: Vec<(u32, u32)> = proxy
                .faces
                .iter()
                .flat_map(|f| {
                    let c: Vec<u32> = corners(f).collect();
                    (0..c.len()).map(move |i| (c[i], c[(i + 1) % c.len()]))
                })
                .collect();
            fit::smooth_weights(&mut weights, &edges, GARMENT_SMOOTHING);
        }
        let normals = normal_sums(&at, &proxy.faces);
        // What the face's units do to what sits on the face — the eyes turn
        // and the lids, brows and lashes move — through the same fitting
        // rule that put it there. Hair hangs on the scalp and stays put.
        let proxy_units: Vec<Vec<(usize, Vec3)>> = if matches!(
            proxy.kind,
            ProxyKind::Eyes | ProxyKind::Eyebrows | ProxyKind::Eyelashes
        ) {
            units
                .iter()
                .map(|unit| proxy_offsets(proxy, unit, base.len()))
                .collect()
        } else {
            Vec::new()
        };
        let moves = proxy_units.iter().any(|u| !u.is_empty());
        // One mesh per tint region, so each takes its own colour.
        let regions = proxy.region_colours.len().max(1) as u8;
        for region in 0..regions {
            let faces: Vec<Face> = if proxy.face_regions.is_empty() {
                proxy.faces.clone()
            } else {
                proxy
                    .faces
                    .iter()
                    .zip(&proxy.face_regions)
                    .filter(|(_, r)| **r == region)
                    .map(|(f, _)| *f)
                    .collect()
            };
            // A bottom worn with a top is tucked under it: a face of the
            // bottom whose corners all hang on skin the top covers is not
            // drawn, or a waistband pokes through a sweater.
            let faces: Vec<Face> = if proxy.kind == ProxyKind::Bottom && !top_covers.is_empty() {
                faces
                    .into_iter()
                    .filter(|f| {
                        !corners(f).all(|v| {
                            top_covers.contains(&main_ref(&proxy.refs[v as usize]))
                        })
                    })
                    .collect()
            } else {
                faces
            };
            if faces.is_empty() {
                continue;
            }
            let (mut part, source) = mesh_part(
                PartKind::Proxy(proxy.kind),
                &proxy.name,
                &at,
                &normals,
                &proxy.uvs,
                &faces,
                |v| weights[v].clone(),
                texture.clone(),
                proxy.alpha,
            );
            if moves {
                part.morphs = morphs(&at, &normals, &faces, &proxy_units, &source);
            }
            part.region = region;
            part.default_tint = proxy.region_colours.get(region as usize).copied();
            part.normal = proxy.normal.clone();
            parts.push(part);
        }
    }

    // The skin's baked detail goes on every part cut from the body.
    for part in parts.iter_mut().filter(|p| p.kind == PartKind::Body) {
        part.normal = skin.and_then(|s| s.normal.clone());
    }

    // The pelvis above the ankles: the unit a clip's hip motion and speed
    // are stored in (`SourceClip::hips`).
    let hip_height = pack
        .bone("pelvis")
        .zip(pack.bone("foot_l"))
        .map_or(1.0, |(p, f)| rest[p].head.y - rest[f].head.y);
    let mut clips: Vec<RetargetedClip> = pack
        .clips
        .iter()
        .map(|c| {
            let r = rig::retarget(pack, &rest, &tpose, c, hip_height);
            RetargetedClip {
                name: c.name.clone(),
                fps: c.fps,
                speed: c.speed * hip_height,
                looped: c.looped,
                rotations: r
                    .local
                    .iter()
                    .map(|frames| frames.iter().map(|q| q.to_array()).collect())
                    .collect(),
                pelvis: pack
                    .bone("pelvis")
                    .map(|p| (p, r.pelvis.iter().map(|v| v.to_array()).collect())),
            }
        })
        .collect();

    plant_feet(&mut clips, &joints, &parts);
    let phone_grip = crate::hands::phone_grip(pack, &rest);
    HumanModel {
        parts,
        joints,
        clips,
        phone_grip,
        height,
        missing_targets,
    }
}

/// Feet: the skin of the body below this height at rest, metres — the soles,
/// heels and toes, what stands on the ground.
const FOOT_HEIGHT: f32 = 0.12;

/// Stand every clip on its soles: lift or lower it, once for the whole clip,
/// so that in a quarter of its frames the lowest point of the feet is at or
/// below the ground and in the rest above it — for a walk, the stance; for a
/// run, the frames that touch down.
///
/// `rig::ground` stands a clip on its lowest *ankle*, which is right only
/// while the foot is flat as it is at rest. With the foot turned, the sole is
/// elsewhere: the idle stood its toes 3.3 cm into the ground, and the walk's
/// lowest sole point wandered from 4.6 cm below to 1.6 cm above (measured
/// 2026-10-19 on the default man by skinning every frame). One frame's
/// minimum would lift the whole clip for the sake of a toe dipping at
/// push-off, which no rigid foot avoids; the quartile is what a foot planted
/// for half a stride and the other foot's swing make of it.
fn plant_feet(clips: &mut [RetargetedClip], joints: &[Joint], parts: &[Part]) {
    // What is drawn near the ground at rest — the feet, or the shoes on them —
    // as position, joints and weights.
    let feet: Vec<(Vec3, [u16; 4], [f32; 4])> = parts
        .iter()
        .flat_map(|p| {
            p.positions
                .iter()
                .zip(&p.joints)
                .zip(&p.weights)
                .map(|((at, j), w)| (Vec3::from_array(*at), *j, *w))
        })
        .filter(|(at, _, _)| at.y < FOOT_HEIGHT)
        .collect();
    if feet.is_empty() {
        return;
    }
    let inverse: Vec<Mat4> = joints
        .iter()
        .map(|j| Mat4::from_cols_array(&j.inverse_bind))
        .collect();
    for clip in clips.iter_mut() {
        let Some((pelvis, at)) = clip.pelvis.as_mut() else {
            continue;
        };
        let pelvis = *pelvis;
        let frames = at.len();
        let mut lowest = Vec::with_capacity(frames);
        let mut up = Vec3::Y;
        for f in 0..frames {
            let mut world: Vec<Mat4> = Vec::with_capacity(joints.len());
            for (i, j) in joints.iter().enumerate() {
                let r = clip
                    .rotations
                    .get(i)
                    .and_then(|r| r.get(f))
                    .map_or(glam::Quat::from_array(j.rotation), |q| {
                        glam::Quat::from_array(*q)
                    });
                let t = if i == pelvis {
                    Vec3::from_array(at[f])
                } else {
                    Vec3::from_array(j.translation)
                };
                let local = Mat4::from_rotation_translation(r, t);
                world.push(j.parent.map_or(local, |p| world[p] * local));
            }
            if f == 0 {
                // A world lift as a move in the pelvis's parent's frame.
                if let Some(p) = joints[pelvis].parent {
                    up = world[p].inverse().transform_vector3(Vec3::Y);
                }
            }
            let low = feet
                .iter()
                .map(|(at, j, w)| {
                    (0..4)
                        .map(|k| {
                            let b = j[k] as usize;
                            (world[b] * inverse[b]).transform_point3(*at) * w[k]
                        })
                        .sum::<Vec3>()
                        .y
                })
                .fold(f32::INFINITY, f32::min);
            lowest.push(low);
        }
        lowest.sort_by(f32::total_cmp);
        let Some(&quartile) = lowest.get(frames / 4) else {
            continue;
        };
        if !quartile.is_finite() {
            continue;
        }
        let lift = up * -quartile;
        for p in at.iter_mut() {
            *p = (Vec3::from_array(*p) + lift).to_array();
        }
    }
}

impl HumanModel {
    pub fn joint(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|j| j.name == name)
    }

    /// Every joint's position relative to the model at one frame of a clip
    /// (`None`: the rest pose). For checking a clip, not for drawing it.
    #[must_use]
    pub fn joint_positions(&self, clip: Option<(usize, usize)>) -> Vec<Vec3> {
        self.joint_poses(clip).into_iter().map(|(t, _)| t).collect()
    }

    /// Every joint's position and rotation relative to the model, as
    /// [`Self::joint_positions`].
    #[must_use]
    pub fn joint_poses(&self, clip: Option<(usize, usize)>) -> Vec<(Vec3, glam::Quat)> {
        let mut world: Vec<(Vec3, glam::Quat)> = Vec::with_capacity(self.joints.len());
        for (i, j) in self.joints.iter().enumerate() {
            let mut t = Vec3::from_array(j.translation);
            let mut r = glam::Quat::from_array(j.rotation);
            if let Some((c, f)) = clip {
                let c = &self.clips[c];
                if let Some(q) = c.rotations.get(i).and_then(|r| r.get(f)) {
                    r = glam::Quat::from_array(*q);
                }
                if let Some((p, at)) = &c.pelvis {
                    if *p == i {
                        t = Vec3::from_array(at[f]);
                    }
                }
            }
            world.push(match j.parent {
                Some(p) => {
                    let (pt, pr) = world[p];
                    (pt + pr * t, pr * r)
                }
                None => (t, r),
            });
        }
        world
    }
}

fn corners(f: &Face) -> impl Iterator<Item = u32> + '_ {
    f.v.iter().copied().filter(|&v| v != Face::NONE)
}

/// The skin a garment lies over, from the base vertices it hangs on, which
/// are scattered (a garment vertex hangs on the three nearest, and the
/// garment is coarser than the body): those grown by [`CLOSE_RINGS`] rings of
/// the body's faces and shrunk back by as many — a closing, which fills the
/// gaps between them without reaching past the hem.
fn close_over(hung: HashSet<u32>, faces: &[Face]) -> HashSet<u32> {
    if hung.is_empty() {
        return hung;
    }
    let mut neighbours: HashMap<u32, Vec<u32>> = HashMap::new();
    for f in faces {
        let c: Vec<u32> = corners(f).collect();
        for &a in &c {
            neighbours.entry(a).or_default().extend(c.iter().copied());
        }
    }
    let mut set = hung;
    for _ in 0..CLOSE_RINGS {
        let grown: Vec<u32> = set
            .iter()
            .flat_map(|v| neighbours.get(v).into_iter().flatten().copied())
            .collect();
        set.extend(grown);
    }
    for _ in 0..CLOSE_RINGS {
        set = set
            .iter()
            .copied()
            .filter(|v| {
                neighbours
                    .get(v)
                    .is_none_or(|n| n.iter().all(|w| set.contains(w)))
            })
            .collect();
    }
    set
}

/// How far [`close_over`] closes, in rings of body faces.
const CLOSE_RINGS: usize = 3;

/// The base-mesh vertex a garment vertex hangs on most.
fn main_ref(r: &crate::ProxyRef) -> u32 {
    let w = r.w();
    let k = (0..3).max_by(|&a, &b| w[a].total_cmp(&w[b])).unwrap_or(0);
    r.v[k]
}

/// Per position, the sum of its faces' normals, each as long as twice the
/// face's area — a smooth normal before it is normalised.
fn normal_sums(positions: &[Vec3], faces: &[Face]) -> Vec<Vec3> {
    let mut sum = vec![Vec3::ZERO; positions.len()];
    for f in faces {
        for t in f.triangles() {
            let [a, b, c] = t.map(|k| positions[f.v[k] as usize]);
            let n = (b - a).cross(c - a);
            for k in t {
                sum[f.v[k] as usize] += n;
            }
        }
    }
    sum
}

/// One unit's offsets on the base mesh as offsets on a proxy's vertices,
/// through the proxy's fitting rule (`fit::fit_vertex`, whose scaled offset
/// a unit does not change).
fn proxy_offsets(proxy: &Proxy, unit: &[(usize, Vec3)], base_len: usize) -> Vec<(usize, Vec3)> {
    let mut at = vec![Vec3::ZERO; base_len];
    for &(i, d) in unit {
        at[i] = d;
    }
    proxy
        .refs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let w = r.w();
            let d: Vec3 = (0..3).map(|k| w[k] * at[r.v[k] as usize]).sum();
            (d != Vec3::ZERO).then_some((i, d))
        })
        .collect()
}

/// The morph targets of a part made by [`mesh_part`]: per unit, each
/// vertex's change of position and of normal. `faces` must include every
/// face a unit moves a corner of; a face none moves keeps its normal, so it
/// may be left out. `normal_sum` is [`normal_sums`] over all the faces that
/// share the part's positions, and `source` the part's position per vertex.
fn morphs(
    positions: &[Vec3],
    normal_sum: &[Vec3],
    faces: &[Face],
    units: &[Vec<(usize, Vec3)>],
    source: &[u32],
) -> Vec<Morph> {
    let mut delta = vec![Vec3::ZERO; positions.len()];
    let mut turn = vec![Vec3::ZERO; positions.len()];
    units
        .iter()
        .map(|unit| {
            for &(i, d) in unit {
                delta[i] = d;
            }
            let mut touched: Vec<usize> = Vec::new();
            for f in faces {
                if !corners(f).any(|v| delta[v as usize] != Vec3::ZERO) {
                    continue;
                }
                for t in f.triangles() {
                    let ix = t.map(|k| f.v[k] as usize);
                    let [a, b, c] = ix.map(|i| positions[i]);
                    let [a2, b2, c2] = ix.map(|i| positions[i] + delta[i]);
                    let change = (b2 - a2).cross(c2 - a2) - (b - a).cross(c - a);
                    for i in ix {
                        turn[i] += change;
                        touched.push(i);
                    }
                }
            }
            let morph = Morph {
                positions: source
                    .iter()
                    .map(|&s| delta[s as usize].to_array())
                    .collect(),
                normals: source
                    .iter()
                    .map(|&s| {
                        let (sum, t) = (normal_sum[s as usize], turn[s as usize]);
                        if t == Vec3::ZERO {
                            return [0.0; 3];
                        }
                        ((sum + t).normalize_or(Vec3::Y) - sum.normalize_or(Vec3::Y)).to_array()
                    })
                    .collect(),
            };
            for &(i, _) in unit {
                delta[i] = Vec3::ZERO;
            }
            for i in touched {
                turn[i] = Vec3::ZERO;
            }
            morph
        })
        .collect()
}

/// One mesh from indexed faces whose corners carry separate position and UV
/// indices: a vertex per distinct (position, UV) pair, normals smoothed per
/// *position* (`normal_sum`, from [`normal_sums`]) so a UV seam is not a
/// shading seam. Also returns each vertex's position index.
#[allow(clippy::too_many_arguments)]
fn mesh_part(
    kind: PartKind,
    name: &str,
    positions: &[Vec3],
    normal_sum: &[Vec3],
    uvs: &[[f32; 2]],
    faces: &[Face],
    influences: impl Fn(usize) -> Vec<(u16, f32)>,
    texture: Option<String>,
    alpha: bool,
) -> (Part, Vec<u32>) {
    let mut source = Vec::new();
    let mut index_of: HashMap<(u32, u32), u32> = HashMap::new();
    let mut part = Part {
        kind,
        name: name.to_string(),
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
        joints: Vec::new(),
        weights: Vec::new(),
        texture,
        alpha,
        region: 0,
        default_tint: None,
        morphs: Vec::new(),
        normal: None,
    };
    for f in faces {
        for t in f.triangles() {
            for k in t {
                let key = (f.v[k], f.uv[k]);
                let ix = *index_of.entry(key).or_insert_with(|| {
                    let v = f.v[k] as usize;
                    let uv = uvs.get(f.uv[k] as usize).copied().unwrap_or_default();
                    let (j, w) = fit::top_four(influences(v));
                    source.push(v as u32);
                    part.positions.push(positions[v].to_array());
                    part.normals
                        .push(normal_sum[v].normalize_or(Vec3::Y).to_array());
                    part.uvs.push([uv[0], 1.0 - uv[1]]);
                    part.joints.push(j);
                    part.weights.push(w);
                    (part.positions.len() - 1) as u32
                });
                part.indices.push(ix);
            }
        }
    }
    (part, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quad whose corners share positions but not UVs splits into separate
    /// vertices, and both triangles face +Y.
    #[test]
    fn a_uv_seam_splits_vertices_but_not_normals() {
        let positions = vec![
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
        ];
        let uvs = vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.5, 0.5]];
        let faces = vec![
            Face {
                v: [0, 1, 2, 3],
                uv: [0, 1, 2, 3],
            },
            Face {
                v: [0, 1, 2, Face::NONE],
                uv: [4, 1, 2, Face::NONE],
            },
        ];
        let (part, _) = mesh_part(
            PartKind::Body,
            "t",
            &positions,
            &normal_sums(&positions, &faces),
            &uvs,
            &faces,
            |_| vec![(0, 1.0)],
            None,
            false,
        );
        assert_eq!(part.indices.len(), 9);
        assert_eq!(part.positions.len(), 5, "corner 0 twice, for its two UVs");
        for n in &part.normals {
            assert!((Vec3::from_array(*n) - Vec3::Y).length() < 1.0e-5, "{n:?}");
        }
        assert_eq!(part.uvs[1], [0.0, 0.0], "v flips to wgpu's convention");
    }

    /// The pack the client ships, or `None` in a checkout without Git LFS
    /// (where the file is a 130-byte pointer).
    fn shipped_pack() -> Option<HumanPack> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/models/avatar/human/default.humanpack"
        );
        let bytes = std::fs::read(path).ok().filter(|b| b.len() > 4096)?;
        Some(HumanPack::decode(&bytes).expect("the shipped pack decodes"))
    }

    fn man(pack: &HumanPack) -> HumanModel {
        build(
            pack,
            &Look {
                phenotype: Phenotype::male(),
                skin: "young_caucasian_male".into(),
                proxies: [
                    "high-poly",
                    "eyebrow001",
                    "eyelashes01",
                    "short02",
                    "male_casualsuit01",
                    "shoes06",
                ]
                .map(String::from)
                .to_vec(),
                eyes: None,
                modifiers: vec![],
            },
        )
    }

    /// A man of a man's height, standing on the ground, his skeleton inside
    /// him: the head above the hips above the knees above the toes.
    #[test]
    fn the_shipped_pack_makes_a_man_who_stands_up() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        assert!(m.missing_targets.is_empty(), "{:?}", m.missing_targets);
        assert!((1.6..1.95).contains(&m.height), "height {}", m.height);
        // The outfit is two meshes, its top and its bottom, tinted apart.
        assert_eq!(
            m.parts.len(),
            9,
            "the body, the face, five proxies and the outfit's two halves"
        );
        let rest = m.joint_positions(None);
        let y = |n: &str| rest[m.joint(n).unwrap()].y;
        assert!(y("head") > y("pelvis") && y("pelvis") > y("calf_l") && y("calf_l") > y("ball_l"));
        assert!(
            y("ball_l") < 0.12,
            "toes on the ground, got {}",
            y("ball_l")
        );
        // Left is +x for a body facing +z.
        assert!(rest[m.joint("hand_l").unwrap()].x > 0.2);
    }

    /// A hand round a phone: each palm faces in toward the body at rest, the
    /// fingers close, and the right arm of the idle can be brought to a phone
    /// held in front of the chest, its palm turned to face the phone's back.
    #[test]
    fn a_hand_can_hold_a_phone_in_front_of_the_chest() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let rest = m.joint_poses(None);
        let [Some(left), Some(right)] = &m.phone_grip else {
            panic!("both hands have a grip")
        };
        for (grip, inward) in [(left, -1.0), (right, 1.0)] {
            let (_, rot) = rest[grip.hand];
            let palm = rot * grip.palm;
            assert!(palm.x * inward > 0.3, "palm {palm} should face x {inward}");
            assert!(grip.fingers.len() >= 12, "{}", grip.fingers.len());
            assert!(grip.palm.dot(grip.towards_fingers).abs() < 1e-4);
        }
        let idle = m.clips.iter().position(|c| c.name == "idle").unwrap();
        let pose = m.joint_poses(Some((idle, 0)));
        let at = |n: &str| pose[m.joint(n).unwrap()].0;
        let arm = crate::reach::Arm {
            shoulder: at("upperarm_r"),
            elbow: at("lowerarm_r"),
            wrist: at("hand_r"),
        };
        let phone = arm.shoulder + Vec3::new(0.12, -0.25, 0.32);
        let r = crate::reach::reach(&arm, phone, Vec3::new(-1.0, -1.0, -0.3));
        assert!(r.reached, "a phone 43 cm from the shoulder is in reach");
        let elbow = arm.shoulder + r.upper * (arm.elbow - arm.shoulder);
        let wrist = elbow + r.lower * r.upper * (arm.wrist - arm.elbow);
        assert!(wrist.distance(phone) < 1e-3, "{wrist} {phone}");
        assert!(elbow.y < arm.shoulder.y - 0.15, "the elbow hangs: {elbow}");
    }

    /// A face control moves the face, and only it: a wider nose changes the
    /// skin by millimetres around the nose and leaves the feet where they were.
    #[test]
    fn a_face_control_moves_the_face() {
        let Some(pack) = shipped_pack() else { return };
        let plain = man(&pack);
        let mut look = Look {
            phenotype: Phenotype::male(),
            skin: "young_caucasian_male".into(),
            proxies: vec![],
            eyes: None,
            modifiers: vec![("nose/nose-scale-horiz-decr-incr".into(), 1.0)],
        };
        let wide = build(&pack, &look);
        assert!(
            wide.missing_targets.is_empty(),
            "{:?}",
            wide.missing_targets
        );
        look.modifiers.clear();
        let base = build(&pack, &look);
        // The skin is the body and, once the pack has face units, the face.
        let skin = |m: &HumanModel| -> Vec<[f32; 3]> {
            m.parts
                .iter()
                .filter(|p| p.kind == PartKind::Body)
                .flat_map(|p| p.positions.iter().copied())
                .collect()
        };
        let (a, b) = (&skin(&base), &skin(&wide));
        let moved: Vec<f32> = a
            .iter()
            .zip(b)
            .map(|(p, q)| (Vec3::from_array(*p) - Vec3::from_array(*q)).length())
            .collect();
        let most = moved.iter().copied().fold(0.0, f32::max);
        assert!(most > 0.002, "the nose widens by millimetres, got {most}");
        let lowest = |m: &HumanModel| {
            m.parts[0]
                .positions
                .iter()
                .map(|p| p[1])
                .fold(f32::MAX, f32::min)
        };
        assert!(
            (lowest(&base) - lowest(&wide)).abs() < 1.0e-4,
            "the feet stay on the ground"
        );
        let _ = plain;
    }

    /// The face's units are live morph targets on the parts they move, and
    /// they move the right things: a blink closes the left lid downwards and
    /// takes the lashes with it, an open jaw lowers the chin by centimetres,
    /// and a look to the left turns the eyes and nothing on the skin.
    #[test]
    fn the_face_units_move_the_face() {
        let Some(pack) = shipped_pack() else { return };
        assert_eq!(pack.face_units.len(), crate::face::UNITS.len());
        let m = man(&pack);
        let part = |name: &str| m.parts.iter().find(|p| p.name == name).unwrap();
        let unit = |name: &str| crate::face::unit(name).unwrap();
        let (face, lashes, eyes) = (part("face"), part("eyelashes01"), part("high-poly"));
        assert!(
            part("body").morphs.is_empty(),
            "the rest of the body has none"
        );
        for p in [face, lashes, eyes] {
            assert_eq!(p.morphs.len(), 52, "{}", p.name);
            for t in &p.morphs {
                assert_eq!(t.positions.len(), p.positions.len());
                assert_eq!(t.normals.len(), p.positions.len());
            }
        }
        // The face is a few thousand vertices: 52 targets on each.
        assert!(face.positions.len() < 6000, "face {}", face.positions.len());
        // Largest movement down (−y) and its x, over a part, for a unit.
        let down = |p: &Part, u: &str| {
            p.morphs[unit(u)]
                .positions
                .iter()
                .zip(&p.positions)
                .map(|(d, at)| (-d[1], at[0]))
                .fold((0.0f32, 0.0f32), |a, b| if b.0 > a.0 { b } else { a })
        };
        let (lid, x) = down(face, "eyeBlinkLeft");
        assert!(
            lid > 0.004,
            "the upper lid comes down by millimetres: {lid}"
        );
        assert!(x > 0.0, "on the face's left, +x: {x}");
        assert!(
            down(lashes, "eyeBlinkLeft").0 > 0.003,
            "the lashes go with it"
        );
        let (chin, _) = down(face, "jawOpen");
        assert!(
            chin > 0.01,
            "an open jaw lowers the chin by centimetres: {chin}"
        );
        let moved = |p: &Part, u: &str| {
            p.morphs[unit(u)]
                .positions
                .iter()
                .map(|d| Vec3::from_array(*d).length())
                .fold(0.0, f32::max)
        };
        assert!(moved(eyes, "eyeLookOutLeft") > 0.001, "the eyes turn");
        // Normals turn with the faces, and stay unit vectors at full weight.
        let t = &face.morphs[unit("jawOpen")];
        assert!(t.normals.iter().any(|n| *n != [0.0; 3]));
        for (n, dn) in face.normals.iter().zip(&t.normals) {
            let turned = Vec3::from_array(*n) + Vec3::from_array(*dn);
            assert!((turned.length() - 1.0).abs() < 1.0e-3);
        }
    }

    /// Retargeting through the T-pose leaves the arms where the library put
    /// them — by the sides when idle, not held out as in either rest pose —
    /// and keeps the feet on the ground through a walk.
    #[test]
    fn the_shipped_clips_play_like_people() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let clip = |name: &str| m.clips.iter().position(|c| c.name == name).unwrap();
        let idle = m.joint_positions(Some((clip("idle"), 0)));
        let (hand, shoulder) = (
            idle[m.joint("hand_l").unwrap()],
            idle[m.joint("upperarm_l").unwrap()],
        );
        assert!(
            shoulder.y - hand.y > 0.4,
            "idle hands hang below the shoulders: hand {hand}, shoulder {shoulder}"
        );
        assert!(
            hand.x < 0.45,
            "and close to the body, not out to the side: {hand}"
        );

        let walk = clip("walk");
        let frames = m.clips[walk].rotations[0].len();
        let mut lowest = f32::INFINITY;
        let mut pelvis = (f32::INFINITY, f32::NEG_INFINITY);
        for f in 0..frames {
            let p = m.joint_positions(Some((walk, f)));
            lowest = lowest.min(
                p[m.joint("foot_l").unwrap()]
                    .y
                    .min(p[m.joint("foot_r").unwrap()].y),
            );
            let py = p[m.joint("pelvis").unwrap()].y;
            pelvis = (pelvis.0.min(py), pelvis.1.max(py));
        }
        assert!(
            lowest < 0.15 && lowest > -0.05,
            "an ankle reaches the ground: {lowest}"
        );
        assert!(
            pelvis.0 > 0.75 && pelvis.1 < 1.1,
            "the hips stay at hip height: {pelvis:?}"
        );
    }

    /// Trousers worn with a top lose the faces under it — the waistband the
    /// sweater hid poked through it in the first capture — and keep the legs.
    #[test]
    fn a_bottom_tucks_under_a_top() {
        let Some(pack) = shipped_pack() else { return };
        let triangles = |proxies: &[&str]| {
            let m = build(
                &pack,
                &Look {
                    phenotype: Phenotype::male(),
                    skin: "young_caucasian_male".into(),
                    proxies: proxies.iter().map(|s| s.to_string()).collect(),
                    eyes: None,
                    modifiers: vec![],
                },
            );
            m.parts
                .iter()
                .filter(|p| p.name == "toigo_wool_pants")
                .map(|p| p.indices.len() / 3)
                .sum::<usize>()
        };
        // And the skin under the sweater is not drawn, though the sweater
        // lists none to delete.
        let skin = |proxies: &[&str]| {
            build(
                &pack,
                &Look {
                    phenotype: Phenotype::male(),
                    skin: "young_caucasian_male".into(),
                    proxies: proxies.iter().map(|s| s.to_string()).collect(),
                    eyes: None,
                    modifiers: vec![],
                },
            )
            .parts
            .iter()
            .filter(|p| p.kind == PartKind::Body)
            .map(|p| p.indices.len() / 3)
            .sum::<usize>()
        };
        let bare = skin(&[]);
        let dressed = skin(&["toigo_fisherman_sweater"]);
        // 18 % went (2026-10-28): the head and hands hold most of the skin.
        assert!(dressed * 20 < bare * 17, "{dressed} of {bare} skin triangles");
        let alone = triangles(&["toigo_wool_pants"]);
        let tucked = triangles(&["toigo_fisherman_sweater", "toigo_wool_pants"]);
        assert!(alone > 0, "the trousers are in the pack");
        assert!(tucked < alone, "the sweater hides some: {tucked} of {alone}");
        assert!(tucked * 2 > alone, "but not the legs: {tucked} of {alone}");
    }

    /// Clothes stay in one piece through the clips (`fit::smooth_weights`):
    /// skinned on the CPU at every other frame, no edge of any outfit, top,
    /// bottom or pair of shoes grows by more than 5 cm (the wrap dress grew
    /// 6.8 cm reaching and was left out of the pack). Before the smoothing, the casual suit's
    /// armpit grew 21 cm in the gestures and its crotch 13 cm in the walk —
    /// spikes. The roll and the kick spread the legs further than any outfit
    /// was cut for and are left out. Fails at four smoothing rounds (the
    /// gestures' armpit, 6.4 cm).
    #[test]
    fn clothes_do_not_spike() {
        use glam::Quat;
        let Some(pack) = shipped_pack() else { return };
        let worn: Vec<&str> = pack
            .proxies
            .iter()
            .filter(|p| {
                matches!(
                    p.kind,
                    ProxyKind::Outfit | ProxyKind::Top | ProxyKind::Bottom | ProxyKind::Shoes
                )
            })
            .map(|p| p.name.as_str())
            .collect();
        let mut spikes = Vec::new();
        for name in worn {
            let m = build(
                &pack,
                &Look {
                    phenotype: Phenotype::male(),
                    skin: "young_caucasian_male".into(),
                    proxies: vec!["high-poly".into(), name.into()],
                    eyes: None,
                    modifiers: vec![],
                },
            );
            let inverse: Vec<glam::Mat4> = m
                .joints
                .iter()
                .map(|j| glam::Mat4::from_cols_array(&j.inverse_bind))
                .collect();
            for clip in &m.clips {
                // A skirt spans both legs, so a stride stretches it (5.0 cm
                // walking, 6.6 running) however it is weighted.
                let skirt_striding =
                    name == "female_elegantsuit01" && (clip.name == "walk" || clip.name == "run");
                if clip.name == "roll" || clip.name == "kick" || skirt_striding {
                    continue;
                }
                for f in (0..clip.rotations[0].len()).step_by(2) {
                    let mut world: Vec<glam::Mat4> = Vec::new();
                    for (i, j) in m.joints.iter().enumerate() {
                        let r = clip.rotations.get(i).and_then(|r| r.get(f));
                        let r = Quat::from_array(*r.unwrap_or(&j.rotation));
                        let t = match &clip.pelvis {
                            Some((p, at)) if *p == i => Vec3::from_array(at[f]),
                            _ => Vec3::from_array(j.translation),
                        };
                        let local = glam::Mat4::from_rotation_translation(r, t);
                        world.push(j.parent.map_or(local, |p| world[p] * local));
                    }
                    let skin: Vec<glam::Mat4> =
                        world.iter().zip(&inverse).map(|(w, i)| *w * *i).collect();
                    for part in m.parts.iter().filter(|p| p.kind != PartKind::Body) {
                        let posed: Vec<Vec3> = part
                            .positions
                            .iter()
                            .enumerate()
                            .map(|(v, p)| {
                                (0..4)
                                    .map(|k| {
                                        skin[part.joints[v][k] as usize]
                                            .transform_point3(Vec3::from_array(*p))
                                            * part.weights[v][k]
                                    })
                                    .sum()
                            })
                            .collect();
                        for t in part.indices.chunks(3) {
                            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                                let (a, b) = (a as usize, b as usize);
                                let rest = Vec3::from_array(part.positions[a])
                                    .distance(Vec3::from_array(part.positions[b]));
                                let grown = posed[a].distance(posed[b]) - rest;
                                if grown >= 0.05 {
                                    let worst = spikes
                                        .iter_mut()
                                        .find(|(n, c, _)| *n == name && *c == clip.name);
                                    match worst {
                                        Some((_, _, g)) => *g = f32::max(*g, grown),
                                        None => spikes.push((name, clip.name.clone(), grown)),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Every offender at once, worst edge per garment and clip.
        assert!(
            spikes.is_empty(),
            "an edge grew past 5 cm:\n{}",
            spikes
                .iter()
                .map(|(n, c, g)| format!("  {n} in {c}: {g:.3} m"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// No upright clip shrugs (`rig::settle_shoulders`): on average the
    /// shoulder joints sit a little lower under the neck than at rest, whose
    /// arms are raised. Every CMU clip held them 8–10 cm higher, level with it.
    #[test]
    fn upright_clips_keep_the_shoulders_down() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let j = |n: &str| m.joint(n).unwrap();
        let below_neck = |p: &[Vec3]| {
            let neck = p[j("neck_01")].y;
            [neck - p[j("upperarm_l")].y, neck - p[j("upperarm_r")].y]
        };
        let rest = below_neck(&m.joint_positions(None));
        for name in ["idle", "walk", "run", "interact", "punch", "sit"] {
            let c = m.clips.iter().position(|c| c.name == name).unwrap();
            let frames = m.clips[c].rotations[0].len();
            for side in 0..2 {
                let mean = (0..frames)
                    .map(|f| below_neck(&m.joint_positions(Some((c, f))))[side])
                    .sum::<f32>()
                    / frames as f32;
                assert!(
                    (0.005..0.05).contains(&(mean - rest[side])),
                    "{name}: a shoulder {mean:.3} m below the neck, at rest {:.3}",
                    rest[side]
                );
            }
        }
    }

    /// Feet on the ground (`plant_feet`): skinned on the CPU, the lowest point
    /// of the body and its shoes is within 2 cm of the ground in a typical
    /// frame of every clip that stands. Grounded on the ankles instead, the
    /// idle stood its toes 3.3 cm into the ground.
    #[test]
    fn standing_clips_put_the_soles_on_the_ground() {
        use glam::Quat;
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let inverse: Vec<Mat4> = m
            .joints
            .iter()
            .map(|j| Mat4::from_cols_array(&j.inverse_bind))
            .collect();
        for name in ["idle", "walk", "interact", "wave", "punch", "sit"] {
            let clip = m.clips.iter().find(|c| c.name == name).unwrap();
            let mut lows: Vec<f32> = (0..clip.rotations[0].len())
                .map(|f| {
                    let mut world: Vec<Mat4> = Vec::new();
                    for (i, j) in m.joints.iter().enumerate() {
                        let r = clip.rotations.get(i).and_then(|r| r.get(f));
                        let r = Quat::from_array(*r.unwrap_or(&j.rotation));
                        let t = match &clip.pelvis {
                            Some((p, at)) if *p == i => Vec3::from_array(at[f]),
                            _ => Vec3::from_array(j.translation),
                        };
                        let local = Mat4::from_rotation_translation(r, t);
                        world.push(j.parent.map_or(local, |p| world[p] * local));
                    }
                    let mut low = f32::MAX;
                    for part in &m.parts {
                        for (v, p) in part.positions.iter().enumerate() {
                            let y = (0..4)
                                .map(|k| {
                                    let b = part.joints[v][k] as usize;
                                    (world[b] * inverse[b]).transform_point3(Vec3::from_array(*p))
                                        * part.weights[v][k]
                                })
                                .sum::<Vec3>()
                                .y;
                            low = low.min(y);
                        }
                    }
                    low
                })
                .collect();
            lows.sort_by(f32::total_cmp);
            let typical = lows[lows.len() / 2];
            assert!(
                typical.abs() < 0.02,
                "{name}: the soles are {typical:.3} m off the ground"
            );
        }
    }

    /// The walk is an ordinary one (CMU 08_08, `human-pack`): the trunk
    /// upright, the arms swinging forward and back of the body against the
    /// legs. The walk it replaced (100STYLE `Rushed`) leaned the trunk 20° and
    /// held the arms 15–47° behind the body; patching it in code still looked
    /// like a funny walk.
    #[test]
    fn the_walk_is_an_ordinary_walk() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let walk = m.clips.iter().position(|c| c.name == "walk").unwrap();
        let frames = m.clips[walk].rotations[0].len();
        let j = |n: &str| m.joint(n).unwrap();
        // Forward of straight down (a limb) or of upright (the trunk), in
        // degrees; the body faces +z.
        let pitch = |d: Vec3| d.z.atan2(-d.y).to_degrees();
        let (mut lo, mut hi, mut lean, mut opposed) = (f32::MAX, f32::MIN, 0.0, 0);
        for f in 0..frames {
            let p = m.joint_positions(Some((walk, f)));
            let trunk = p[j("neck_01")] - p[j("pelvis")];
            lean += trunk.z.atan2(trunk.y).to_degrees() / frames as f32;
            let arm = pitch(p[j("lowerarm_l")] - p[j("upperarm_l")]);
            lo = lo.min(arm);
            hi = hi.max(arm);
            if arm * pitch(p[j("calf_l")] - p[j("thigh_l")]) < 0.0 {
                opposed += 1;
            }
        }
        assert!((-2.0..8.0).contains(&lean), "the trunk leans {lean:.1}°");
        assert!(
            lo < -10.0 && hi > 10.0,
            "the arm swings forward and back of the body: {lo:.0}° to {hi:.0}°"
        );
        assert!(
            opposed * 3 > frames * 2,
            "the arm opposes its leg in {opposed} of {frames} frames"
        );
    }

    /// The punch closes the hand into a fist and the wave opens it: the
    /// fingertips come in toward the thumb's root — the palm side, so a curl
    /// the wrong way round would move them away — and the wave's are further
    /// out than at rest.
    #[test]
    fn a_punch_makes_a_fist_and_a_wave_an_open_hand() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let clip = |name: &str| m.clips.iter().position(|c| c.name == name).unwrap();
        let reach = |pose: Option<(usize, usize)>| {
            let p = m.joint_positions(pose);
            let palm = p[m.joint("thumb_01_l").unwrap()];
            ["index_03_l", "middle_03_l", "ring_03_l", "pinky_03_l"]
                .iter()
                .map(|n| p[m.joint(n).unwrap()].distance(palm))
                .sum::<f32>()
        };
        let rest = reach(None);
        let fist = reach(Some((clip("punch"), 0)));
        let open = reach(Some((clip("wave"), 0)));
        assert!(fist < rest * 0.8, "a fist: {fist} against {rest} at rest");
        assert!(open > rest, "an open hand: {open} against {rest} at rest");
    }

    /// Standing clips look ahead, not at the pavement (`rig::level_gaze`).
    #[test]
    fn standing_clips_look_ahead() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let head = m.joint("head").unwrap();
        // The joints' world rotations for a frame of `clip`.
        let world = |c: Option<usize>, f: usize| {
            let mut w = vec![glam::Quat::IDENTITY; m.joints.len()];
            for (i, j) in m.joints.iter().enumerate() {
                let r = c.map_or(j.rotation, |c| m.clips[c].rotations[i][f]);
                let r = glam::Quat::from_array(r);
                w[i] = j.parent.map_or(r, |p| w[p] * r);
            }
            w
        };
        let rest = world(None, 0)[head];
        for name in ["idle", "walk", "wave", "sit"] {
            let c = m.clips.iter().position(|c| c.name == name).unwrap();
            let frames = m.clips[c].rotations[head].len();
            let mean = (0..frames)
                .map(|f| {
                    (world(Some(c), f)[head] * rest.inverse() * glam::Vec3::Z)
                        .y
                        .asin()
                })
                .sum::<f32>()
                / frames as f32;
            assert!(
                (mean.to_degrees() + 6.0).abs() < 1.0,
                "{name} looks {:.1}° from level",
                mean.to_degrees()
            );
        }
    }
}

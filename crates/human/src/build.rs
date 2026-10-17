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
    /// Top of the head above the soles, metres.
    pub height: f32,
    /// Targets the phenotype asked for that the pack does not carry — a pack
    /// cut down to a few bodies, asked for another.
    pub missing_targets: Vec<String>,
}

/// Build the person `look` describes.
#[must_use]
pub fn build(pack: &HumanPack, look: &Look) -> HumanModel {
    // Shape, in the OBJ frame the targets and the fitting rules are written in.
    let mut base: Vec<Vec3> = pack.vertices.iter().map(|&v| Vec3::from_array(v)).collect();
    let mut missing_targets = Vec::new();
    let by_name: HashMap<&str, &crate::Target> =
        pack.targets.iter().map(|t| (t.name.as_str(), t)).collect();
    for (name, w) in macros::macro_targets(&look.phenotype) {
        match by_name.get(name.as_str()) {
            Some(t) => {
                for (&i, d) in t.indices.iter().zip(&t.deltas) {
                    base[i as usize] += w * Vec3::from_array(*d);
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

    // The skin, less what the clothes cover.
    let covered: HashSet<u32> = proxies
        .iter()
        .flat_map(|p| p.delete_verts.iter().copied())
        .collect();
    let body_faces: Vec<Face> = pack
        .body_faces
        .iter()
        .filter(|f| !corners(f).all(|v| covered.contains(&v)))
        .copied()
        .collect();
    let skin = pack.skins.iter().find(|s| s.name == look.skin);
    let mut parts = vec![mesh_part(
        PartKind::Body,
        "body",
        &positions,
        &pack.uvs,
        &body_faces,
        |v| pack.weights[v].clone(),
        skin.map(|s| s.texture.clone()),
        false,
    )];
    for (proxy, fitted) in proxies.iter().zip(&fitted) {
        let at: Vec<Vec3> = fitted.iter().map(|&v| to_metres(v)).collect();
        parts.push(mesh_part(
            PartKind::Proxy(proxy.kind),
            &proxy.name,
            &at,
            &proxy.uvs,
            &proxy.faces,
            |v| fit::blend_weights(&proxy.refs[v], &pack.weights),
            proxy.texture.clone(),
            proxy.alpha,
        ));
    }

    let hip_height = pack.bone("pelvis").map_or(1.0, |p| rest[p].head.y);
    let clips = pack
        .clips
        .iter()
        .map(|c| {
            let r = rig::retarget(pack, &rest, &tpose, c, hip_height);
            RetargetedClip {
                name: c.name.clone(),
                fps: c.fps,
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

    HumanModel {
        parts,
        joints,
        clips,
        height,
        missing_targets,
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
        world.into_iter().map(|(t, _)| t).collect()
    }
}

fn corners(f: &Face) -> impl Iterator<Item = u32> + '_ {
    f.v.iter().copied().filter(|&v| v != Face::NONE)
}

/// One mesh from indexed faces whose corners carry separate position and UV
/// indices: a vertex per distinct (position, UV) pair, normals smoothed per
/// *position* so a UV seam is not a shading seam.
#[allow(clippy::too_many_arguments)]
fn mesh_part(
    kind: PartKind,
    name: &str,
    positions: &[Vec3],
    uvs: &[[f32; 2]],
    faces: &[Face],
    influences: impl Fn(usize) -> Vec<(u16, f32)>,
    texture: Option<String>,
    alpha: bool,
) -> Part {
    let mut normal_at = vec![Vec3::ZERO; positions.len()];
    for f in faces {
        for t in f.triangles() {
            let [a, b, c] = t.map(|k| positions[f.v[k] as usize]);
            // Area-weighted: the cross product's length is twice the area.
            let n = (b - a).cross(c - a);
            for k in t {
                normal_at[f.v[k] as usize] += n;
            }
        }
    }
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
    };
    for f in faces {
        for t in f.triangles() {
            for k in t {
                let key = (f.v[k], f.uv[k]);
                let ix = *index_of.entry(key).or_insert_with(|| {
                    let v = f.v[k] as usize;
                    let uv = uvs.get(f.uv[k] as usize).copied().unwrap_or_default();
                    let (j, w) = fit::top_four(influences(v));
                    part.positions.push(positions[v].to_array());
                    part.normals
                        .push(normal_at[v].normalize_or(Vec3::Y).to_array());
                    part.uvs.push([uv[0], 1.0 - uv[1]]);
                    part.joints.push(j);
                    part.weights.push(w);
                    (part.positions.len() - 1) as u32
                });
                part.indices.push(ix);
            }
        }
    }
    part
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
        let part = mesh_part(
            PartKind::Body,
            "t",
            &positions,
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
                skin: "male".into(),
                proxies: [
                    "high-poly",
                    "eyebrow001",
                    "eyelashes01",
                    "short02",
                    "male_casualsuit06",
                    "shoes06",
                ]
                .map(String::from)
                .to_vec(),
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
        assert_eq!(m.parts.len(), 7, "the body and six proxies");
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

    /// Retargeting through the T-pose leaves the arms where the library put
    /// them — by the sides when idle, not held out as in either rest pose —
    /// and keeps the feet on the ground through a walk.
    #[test]
    fn the_shipped_clips_play_like_people() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let clip = |name: &str| m.clips.iter().position(|c| c.name == name).unwrap();
        let idle = m.joint_positions(Some((clip("Idle_Loop"), 0)));
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

        let walk = clip("Walk_Loop");
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
}
